//! SSH в окне без gpui (F03-05, F03 §2.8): строки группы «SSH» меню дисков, подписи
//! «когда подключались», тексты статуса и клавиш строк подключения, раздел
//! «Подключения» настроек. Данные — [`urfin_net::NetState`] в `AppState.ext`; рисует
//! `gpui_shell::ssh`.

use urfin_core::{AppState, Command, Side, Tab};
use urfin_i18n::FluentArgs;
use urfin_net::live::{self, AskKind, HintSource, HostHint, LiveState};
use urfin_net::{NetState, Profile, ProfileSource, SftpUri};

use crate::fmt_cell::FmtCfg;

/// Состояние SSH.
pub fn net(s: &AppState) -> Option<&NetState> {
    s.ext.get::<NetState>()
}

/// Профиль таба `sftp://`.
pub fn profile_of(t: &Tab) -> Option<String> {
    SftpUri::from_location(&t.loc).map(|u| u.profile)
}

/// Пинг панели `sftp://`, мс: RTT последнего листинга или keep-alive.
pub fn ping(s: &AppState, t: &Tab) -> Option<u64> {
    let p = profile_of(t)?;
    let d = net(s)?.rtt(&p)?;
    Some(u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Сейчас, нс от Unix epoch.
pub fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

/// Как подписать «когда»: меню дисков — «12 мин назад», «вчера, 18:40»; таблица
/// «Подключений» — «сегодня 09:12», «вчера 18:40», «21.10.26».
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum When {
    Menu,
    Table,
}

/// Подпись времени `secs` (секунды Unix) относительно `now` (нс); `None` — «не
/// подключались» (меню) или «—» (таблица).
pub fn when(fmt: &FmtCfg, secs: Option<i64>, now: i64, how: When) -> String {
    let i18n = urfin_i18n::current();
    let Some(secs) = secs else {
        return match how {
            When::Menu => i18n.get("net-never"),
            When::Table => "—".into(),
        };
    };
    let then = secs.saturating_mul(1_000_000_000);
    let ago = now.saturating_sub(then) / 1_000_000_000;
    let mut a = FluentArgs::new();
    if how == When::Menu && (0..3600).contains(&ago) {
        a.set("n", (ago / 60).max(1));
        return i18n.fmt("net-min-ago", &a);
    }
    let (Some(t), Some(today)) = (fmt.local(then), fmt.local(now)) else {
        return String::new();
    };
    let day = |t: &crate::sys::LocalTime| (t.year, t.month, t.day);
    let yesterday = fmt.local(now - 86_400 * 1_000_000_000);
    a.set("time", format!("{:02}:{:02}", t.hour, t.minute));
    let key = if day(&t) == day(&today) {
        "net-today"
    } else if yesterday.is_some_and(|y| day(&y) == day(&t)) {
        if how == When::Menu {
            "net-yesterday-menu"
        } else {
            "net-yesterday"
        }
    } else {
        return format!("{:02}.{:02}.{:02}", t.day, t.month, t.year.rem_euclid(100));
    };
    i18n.fmt(key, &a)
}

/// Метка источника профиля в строке (`config`, `PuTTY`); свои и разовые — без метки.
pub fn source_tag(p: &Profile) -> Option<&'static str> {
    match p.source {
        ProfileSource::Config => Some("config"),
        ProfileSource::Putty => Some("PuTTY"),
        ProfileSource::Urfin | ProfileSource::History => None,
    }
}

/// Строка группы «SSH» меню дисков (`screen-sshdrives`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshRow {
    pub profile: String,
    /// `admin@web01.corp.local`.
    pub user_host: String,
    /// «12 мин назад» / «не подключались».
    pub when: String,
    pub tag: Option<&'static str>,
    /// Сеанс поднят: зелёная точка у значка.
    pub connected: bool,
}

/// Профили для меню дисков; профили ещё не прочитаны — пусто.
pub fn rows(s: &AppState, fmt: &FmtCfg, now: i64) -> Vec<SshRow> {
    let Some(st) = net(s) else {
        return Vec::new();
    };
    let list = st.profiles.as_ref().map_or(&[][..], |p| &p.list[..]);
    list.iter()
        .map(|p| SshRow {
            profile: p.name.clone(),
            user_host: live::user_host(p),
            when: when(
                fmt,
                p.last_used.as_ref().and_then(live::unix),
                now,
                When::Menu,
            ),
            tag: source_tag(p),
            connected: st
                .live(&p.name)
                .is_some_and(|l| l.state == LiveState::Connected),
        })
        .collect()
}

/// Подпись способа входа (`net-auth-*`), для `Command` — с менеджером паролей.
pub fn auth_label(p: &Profile) -> String {
    urfin_i18n::current().get(live::auth_key(p.auth))
}

/// Статус меню дисков на профиле: «Профиль prod-web · вход через агент» слева,
/// «SSH: 1 подключение» справа.
pub fn drive_status(s: &AppState, profile: Option<&str>) -> Option<[String; 3]> {
    let st = net(s)?;
    let i18n = urfin_i18n::current();
    let mut a = FluentArgs::new();
    a.set("n", st.connected());
    let right = i18n.fmt("net-status-connections", &a);
    let left = profile
        .and_then(|n| st.profile(n))
        .map(|p| {
            let mut a = FluentArgs::new();
            a.set("profile", p.name.clone());
            a.set("auth", i18n.get(via_key(p)));
            i18n.fmt("net-status-profile", &a)
        })
        .unwrap_or_default();
    Some([left, String::new(), right])
}

/// «вход через агент», «вход по ключу»…
fn via_key(p: &Profile) -> &'static str {
    match p.auth {
        urfin_net::Auth::Agent => "net-via-agent",
        urfin_net::Auth::Key => "net-via-key",
        urfin_net::Auth::Password => "net-via-password",
        urfin_net::Auth::Command => "net-via-command",
    }
}

/// Клавиши меню дисков на профиле SSH (`screen-sshdrives`).
pub fn drive_keys() -> Vec<(String, String)> {
    let i18n = urfin_i18n::current();
    let k = |key: &str, label: &str| (key.to_owned(), i18n.get(label));
    vec![
        k("Enter", "net-key-connect"),
        k("Ins", "net-key-new"),
        k("F4", "net-key-edit"),
        k("Del", "net-key-disconnect"),
        k("Esc", "ui-keys-close"),
    ]
}

/// Команды пункта SSH меню дисков.
pub fn open_cmds(profile: &str, side: Side) -> Vec<Command> {
    vec![
        Command::new("ui.cancel"),
        Command::new("sftp.open")
            .arg("profile", profile)
            .arg("side", crate::side_arg(side)),
    ]
}

pub fn new_cmds(side: Side) -> Vec<Command> {
    vec![
        Command::new("ui.cancel"),
        Command::new("ssh.reload"),
        Command::new("ssh.new").arg("side", crate::side_arg(side)),
    ]
}

pub fn edit_cmds(profile: &str, side: Side) -> Vec<Command> {
    vec![
        Command::new("ui.cancel"),
        Command::new("ssh.edit")
            .arg("profile", profile)
            .arg("side", crate::side_arg(side)),
    ]
}

/// Источник подсказки «Хоста» справа: «config · Host srv1», «история · вчера»,
/// «`known_hosts`», «профиль web».
pub fn hint_source(h: &HostHint, fmt: &FmtCfg, now: i64) -> String {
    let i18n = urfin_i18n::current();
    let mut a = FluentArgs::new();
    match &h.source {
        HintSource::Config(name) => {
            a.set("name", name.clone());
            i18n.fmt("net-hint-config", &a)
        }
        HintSource::History(t) => {
            let w = match *t {
                Some(secs) => {
                    let s = when(fmt, Some(secs), now, When::Table);
                    // «вчера 18:40» → «вчера»: в подсказке — только день.
                    s.split_whitespace().next().unwrap_or_default().to_owned()
                }
                None => String::new(),
            };
            a.set("when", w);
            i18n.fmt("net-hint-history", &a)
        }
        HintSource::KnownHosts => "known_hosts".into(),
        HintSource::Profile(name) => {
            a.set("name", name.clone());
            i18n.fmt("net-hint-profile", &a)
        }
    }
}

/// Статус строки «Новое подключение»: источники слева, «k из N» посередине.
pub fn form_status(pos: Option<(usize, usize)>) -> [String; 3] {
    let i18n = urfin_i18n::current();
    let mid = pos.map_or_else(String::new, |(k, n)| {
        let mut a = FluentArgs::new();
        a.set("n", k);
        a.set("total", n);
        i18n.fmt("ui-history-count", &a)
    });
    [i18n.get("net-form-sources"), mid, String::new()]
}

/// Клавиши строки «Новое подключение» (`screen-sshnew`).
pub fn form_keys() -> Vec<(String, String)> {
    let i18n = urfin_i18n::current();
    let k = |key: &str, label: &str| (key.to_owned(), i18n.get(label));
    vec![
        k("↑ ↓", "net-key-variant"),
        k("Alt+↓", "net-key-host-history"),
        k("Tab", "ui-keys-next-field"),
        k("← →", "net-key-auth"),
    ]
}

/// Статус вопроса подключения (справа): «Подключение: ключ сервера · 2 из 3»,
/// «Подключение остановлено».
pub fn ask_status(s: &AppState, base: &[String; 3]) -> Option<[String; 3]> {
    let ask = net(s)?.ask()?;
    let i18n = urfin_i18n::current();
    let right = match &ask.kind {
        AskKind::HostChanged { .. } => i18n.get("net-status-stopped"),
        AskKind::HostKey { .. } => i18n.get("net-status-hostkey"),
        AskKind::Secret { .. } => i18n.get("net-status-auth"),
    };
    Some([base[0].clone(), base[1].clone(), right])
}

/// Клавиши вопроса: ключ сервера — Tab и копирование отпечатка; пароль — список
/// шаблонов, когда он открыт.
pub fn ask_keys(s: &AppState, pouch: bool) -> Option<Vec<(String, String)>> {
    let ask = net(s)?.ask()?;
    let i18n = urfin_i18n::current();
    let k = |key: &str, label: &str| (key.to_owned(), i18n.get(label));
    Some(match &ask.kind {
        AskKind::HostKey { .. } | AskKind::HostChanged { .. } => vec![
            k("Tab", "net-key-next-button"),
            k("Ctrl+C", "net-key-copy-fp"),
        ],
        AskKind::Secret { .. } if pouch => vec![
            k("↑ ↓", "ui-keys-choose"),
            k("Enter", "net-key-use-cmd"),
            k("Esc", "net-key-close-list"),
        ],
        AskKind::Secret { .. } => vec![k("Alt+↓", "net-key-manager")],
    })
}

/// Профили раздела «Подключения» под фильтром (имя, хост, пользователь).
pub fn conn_rows<'a>(s: &'a AppState, filter: &str) -> Vec<&'a Profile> {
    let f = filter.trim().to_lowercase();
    net(s)
        .and_then(|n| n.profiles.as_ref())
        .map(|p| {
            p.list
                .iter()
                .filter(|p| {
                    f.is_empty()
                        || [&p.name, &p.host, &p.user]
                            .iter()
                            .any(|v| v.to_lowercase().contains(&f))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Статус раздела «Подключения»: файл · «8 профилей · 5 из ~/.ssh/config · 2 из
/// `PuTTY`» · «Пароли в файл не пишутся».
pub fn conn_status(s: &AppState) -> [String; 3] {
    let i18n = urfin_i18n::current();
    let st = net(s);
    let file = st.and_then(|n| n.files.as_ref()).map_or_else(
        || r"%APPDATA%\Urfin\connections.toml".to_owned(),
        |f| f.connections.display().to_string(),
    );
    let list = st
        .and_then(|n| n.profiles.as_ref())
        .map_or(&[][..], |p| &p.list[..]);
    let count = |src| list.iter().filter(|p| p.source == src).count();
    let mut a = FluentArgs::new();
    a.set("n", list.len());
    a.set("config", count(ProfileSource::Config));
    a.set("putty", count(ProfileSource::Putty));
    [
        file,
        i18n.fmt("net-conn-count", &a),
        i18n.get("net-conn-no-passwords"),
    ]
}

/// Клавиши раздела «Подключения» (`screen-settings-conn`).
pub fn conn_keys() -> Vec<(String, String)> {
    let i18n = urfin_i18n::current();
    let k = |key: &str, label: &str| (key.to_owned(), i18n.get(label));
    vec![
        k("Enter", "net-key-connect-to"),
        k("Shift+Enter", "net-key-terminal"),
        k("Ins", "net-key-add"),
        k("F4", "net-key-change"),
        k("Del", "net-key-remove"),
        k("Esc", "ui-keys-close"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt() -> FmtCfg {
        FmtCfg::new(&urfin_i18n::current(), false, crate::sys::Zone::utc())
    }

    #[test]
    fn when_labels() {
        let f = fmt();
        // 2026-10-23 09:14:00 UTC.
        let now = 1_792_746_840 * 1_000_000_000_i64;
        let at = |ago: i64| Some(1_792_746_840 - ago);
        let i18n = urfin_i18n::current();
        assert_eq!(when(&f, None, now, When::Menu), i18n.get("net-never"));
        assert_eq!(when(&f, None, now, When::Table), "—");
        let mut a = FluentArgs::new();
        a.set("n", 12);
        assert_eq!(
            when(&f, at(12 * 60), now, When::Menu),
            i18n.fmt("net-min-ago", &a)
        );
        let mut a = FluentArgs::new();
        a.set("time", "07:14");
        assert_eq!(
            when(&f, at(7200), now, When::Table),
            i18n.fmt("net-today", &a)
        );
        a.set("time", "18:40");
        let y = at(9 * 3600 + 14 * 60 + 5 * 3600 + 20 * 60);
        assert_eq!(when(&f, y, now, When::Table), i18n.fmt("net-yesterday", &a));
        assert_eq!(
            when(&f, y, now, When::Menu),
            i18n.fmt("net-yesterday-menu", &a)
        );
        assert_eq!(when(&f, at(3 * 86_400), now, When::Table), "20.10.26");
    }

    #[test]
    fn rows_need_profiles() {
        use urfin_core::{Location, Panel, TabId};
        let here = Location::local(r"C:\w");
        let mut s = AppState::new(
            Panel::new(Tab::new(TabId(1), here.clone())),
            Panel::new(Tab::new(TabId(2), here)),
        );
        assert_eq!(rows(&s, &fmt(), 0), Vec::new());
        let mut st = NetState::new(urfin_net::SshCfg::default(), None);
        let mut p = Profile::new("srv1", "srv1.corp.local");
        p.user = "admin".into();
        p.source = ProfileSource::Config;
        let mut ps = urfin_net::Profiles::default();
        ps.list = vec![p, Profile::new("own", "h")];
        st.profiles = Some(ps);
        s.ext.insert(st);
        let r = rows(&s, &fmt(), 0);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].user_host, "admin@srv1.corp.local");
        assert_eq!(r[0].tag, Some("config"));
        assert_eq!((r[1].user_host.as_str(), r[1].tag), ("h", None));
        assert!(!r[0].connected);
        assert_eq!(conn_rows(&s, "ADMIN").len(), 1);
        assert_eq!(conn_rows(&s, "").len(), 2);
        let z = conn_status(&s);
        assert!(z[1].contains('2'), "{z:?}");
    }
}
