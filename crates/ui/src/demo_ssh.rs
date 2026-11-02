//! Фикстура SSH для сцен `--screens` (F03-05): профили, история хостов, `known_hosts`,
//! сеансы и вопросы подключения — без сети и диска. `screens.ssh kind=` —
//! `profiles` (меню дисков, новое подключение), `conn` («Подключения» настроек),
//! `hostkey`, `changed`, `password`; `stop=true` — снять вопросы; `clear` — всё.

use urfin_core::net::{
    Ask, AskKind, Auth, History, Live, LiveState, NetModel, PasswordManager, Profile,
    ProfileSource, PromptKind, datetime,
};
use urfin_core::{Cx, Side};

/// Секунды Unix «сейчас минус `ago`». «Запомнить» под замком (`screen-sshpass`) даёт
/// политика сцен `sys::policy::demo` (`password_cache = 0`, GPO).
fn ago(ago: i64) -> i64 {
    crate::ssh::now_ns() / 1_000_000_000 - ago
}

fn profile(
    name: &str,
    host: &str,
    user: &str,
    src: ProfileSource,
    auth: Auth,
    via: &str,
    last: Option<i64>,
) -> Profile {
    let mut p = Profile::new(name, host);
    user.clone_into(&mut p.user);
    via.clone_into(&mut p.via);
    p.source = src;
    p.auth = auth;
    p.last_used = last.map(datetime);
    p
}

/// Меню дисков (`screen-sshdrives`) и подсказки «Хоста» (`screen-sshnew`).
fn drives_set() -> Vec<Profile> {
    vec![
        profile(
            "prod-web",
            "web01.corp.local",
            "admin",
            ProfileSource::Urfin,
            Auth::Agent,
            "",
            Some(ago(12 * 60)),
        ),
        profile(
            "bastion",
            "bastion.corp.local",
            "a.antropov",
            ProfileSource::Urfin,
            Auth::Agent,
            "",
            Some(ago(86_400)),
        ),
        profile(
            "srv1",
            "srv1.corp.local",
            "admin",
            ProfileSource::Config,
            Auth::Agent,
            "",
            None,
        ),
    ]
}

/// «Подключения» (`screen-settings-conn`).
fn conn_set() -> Vec<Profile> {
    let day = 86_400;
    let mut web = profile(
        "web-prod-01",
        "10.20.1.11",
        "deploy",
        ProfileSource::Config,
        Auth::Key,
        "bastion",
        Some(ago(3600)),
    );
    "~/.ssh/deploy_ed25519".clone_into(&mut web.key);
    "/var/www/portal".clone_into(&mut web.start_dir);
    web.bookmarks = vec!["/var/log/nginx".into(), "/etc/nginx".into()];
    let mut siem = profile(
        "siem-collector",
        "siem01.corp.local",
        "svc-siem",
        ProfileSource::Config,
        Auth::Command,
        "bastion",
        Some(ago(5 * day)),
    );
    PasswordManager::BearPass
        .template()
        .unwrap_or_default()
        .clone_into(&mut siem.password_cmd);
    let mut nas = profile(
        "nas",
        "nas.home.arpa",
        "admin",
        ProfileSource::Putty,
        Auth::Password,
        "",
        Some(ago(21 * day)),
    );
    nas.port = 2222;
    vec![
        profile(
            "bastion",
            "bastion.corp.local",
            "anton",
            ProfileSource::Config,
            Auth::Agent,
            "",
            Some(ago(3700)),
        ),
        web,
        profile(
            "web-prod-02",
            "10.20.1.12",
            "deploy",
            ProfileSource::Config,
            Auth::Key,
            "bastion",
            Some(ago(day)),
        ),
        profile(
            "db-prod",
            "10.20.2.5",
            "anton",
            ProfileSource::Config,
            Auth::Key,
            "bastion",
            Some(ago(2 * day)),
        ),
        siem,
        profile(
            "build-runner",
            "192.168.50.20",
            "ci",
            ProfileSource::Urfin,
            Auth::Agent,
            "",
            None,
        ),
        nas,
        profile(
            "router",
            "192.168.1.1",
            "admin",
            ProfileSource::Putty,
            Auth::Password,
            "",
            Some(ago(40 * day)),
        ),
    ]
}

fn fill(st: &mut NetModel, list: Vec<Profile>) {
    st.profiles = Some(list);
    let mut h = History::default();
    h.touch("srv1-stage.corp.local", "admin", datetime(ago(86_400)));
    st.history = Some(h);
    st.known = [
        "srv1.corp.local",
        "srv1-db.corp.local",
        "srv10.corp.local",
        "web01.corp.local",
    ]
    .map(String::from)
    .to_vec();
    st.host_keys.insert(
        "web-prod-01".into(),
        (
            "ssh-ed25519".into(),
            "SHA256:q7Kc9mR2vT5nB8xJ0dLp3sWq6yZa1cE4hGu7Lw4f3Ua".into(),
        ),
    );
    st.live.retain(|l| l.profile != "prod-web");
    st.live.push(Live::new(
        "prod-web",
        "admin@web01.corp.local",
        LiveState::Connected,
        Side::Left,
    ));
    st.rev += 1;
}

const FP_OLD: &str = "SHA256:Xq3bN0V8qkM2f6wJ1pZr9tYcHq4uLs7eDgA5oKiW0xE";
const FP_NEW: &str = "SHA256:9fLw2RkT0cYv8NqZ3hA6mUe1xPdG5sJ7bKoHi4WtV2c";

/// Вопрос подключения к `srv1` слева, ответ никто не ждёт.
fn ask(st: &mut NetModel, kind: AskKind) {
    st.asks.clear();
    st.form = None;
    if st.live("srv1").is_none() {
        st.live.push(Live::new(
            "srv1",
            "admin@srv1.corp.local",
            LiveState::Connecting,
            Side::Left,
        ));
    }
    st.asks.push_back(Ask::new(
        "srv1",
        "srv1.corp.local",
        "admin@srv1.corp.local",
        kind,
    ));
    st.rev += 1;
}

pub(crate) fn run(kind: &str, stop: bool, cx: &mut Cx<'_>) {
    let Some(st) = cx.state.ext.get_mut::<NetModel>() else {
        return;
    };
    if stop {
        st.asks.clear();
        st.live.retain(|l| l.profile != "srv1");
        st.rev += 1;
        return;
    }
    match kind {
        // Выбор меню дисков и открытый «мешочек» — у окна: событием.
        "pick" | "pouch" => {
            let id = if kind == "pick" {
                "screens.drive_pick"
            } else {
                "screens.ssh.pouch"
            };
            cx.ui(urfin_core::UiEvent::Ext {
                id: urfin_core::CommandId::from_static(id),
                args: urfin_core::Args::default().with("profile", "prod-web"),
            });
        }
        "profiles" => fill(st, drives_set()),
        "conn" => fill(st, conn_set()),
        "hostkey" => ask(
            st,
            AskKind::HostKey {
                algo: "ssh-ed25519".into(),
                fingerprint: FP_OLD.into(),
            },
        ),
        "changed" => ask(
            st,
            AskKind::HostChanged {
                algo: "ssh-ed25519".into(),
                file: "known_hosts".into(),
                line: 14,
                old: FP_OLD.into(),
                fingerprint: FP_NEW.into(),
            },
        ),
        "password" => ask(
            st,
            AskKind::Secret {
                kind: PromptKind::Password,
                text: "admin@srv1.corp.local's password:".into(),
                echo: false,
            },
        ),
        _ => {
            st.asks.clear();
            st.live.clear();
            st.form = None;
            st.profiles = None;
            st.history = None;
            st.known.clear();
            st.host_keys.clear();
            st.rev += 1;
        }
    }
}
