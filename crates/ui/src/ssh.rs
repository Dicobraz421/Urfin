//! SSH в окне без gpui (F03-05, F03 §2.8): строки группы «SSH» меню дисков, подписи
//! «когда подключались», тексты статуса и клавиш строк подключения, раздел
//! «Подключения» настроек. Данные — [`NetModel`] (core) в `AppState.ext`; рисует
//! `gpui_shell::ssh`.

use urfin_core::net::{
    self as live, AskKind, Auth, HintSource, HostHint, LiveState, NetModel, Profile, ProfileSource,
};
use urfin_core::{AppState, Command, Side, Tab};
use urfin_i18n::FluentArgs;

use crate::fmt_cell::FmtCfg;

/// Состояние SSH.
pub fn net(s: &AppState) -> Option<&NetModel> {
    s.ext.get::<NetModel>()
}

/// Профиль таба `sftp://`.
pub fn profile_of(t: &Tab) -> Option<String> {
    live::sftp_profile(&t.loc)
}

/// Пинг панели `sftp://`, мс: RTT последнего листинга или keep-alive.
pub fn ping(s: &AppState, t: &Tab) -> Option<u64> {
    let p = profile_of(t)?;
    let d = net(s)?.rtt(&p)?;
    Some(u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Связь панели `sftp://` (F03-06): от неё — приглушение, «из кэша», строка обрыва.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Link {
    /// Сеанс поднят.
    Up,
    /// Идёт подключение.
    Connecting,
    /// Обрыв: попытка `attempt` из `tries`, следующая — в `retry_at`.
    Lost {
        attempt: u32,
        tries: u32,
        retry_at: Option<std::time::Instant>,
    },
    /// Сеанса нет: отключились, попытки исчерпаны, таб из сессии.
    Off,
}

/// Связь таба; `None` — таб не `sftp://`.
pub fn link(s: &AppState, t: &Tab) -> Option<Link> {
    let p = profile_of(t)?;
    Some(match net(s).and_then(|n| n.live(&p)).map(|l| &l.state) {
        Some(LiveState::Connected) => Link::Up,
        Some(LiveState::Connecting) => Link::Connecting,
        Some(&LiveState::Lost {
            attempt,
            tries,
            retry_at,
        }) => Link::Lost {
            attempt,
            tries,
            retry_at,
        },
        Some(LiveState::Disconnected | LiveState::Failed(_)) | None => Link::Off,
    })
}

/// Листинг таба не прочитан (таб из сессии, ошибка чтения).
fn failed(t: &Tab) -> bool {
    matches!(t.listing.status, urfin_core::ListingStatus::Failed(_))
}

/// Таб `sftp://` без связи и без листинга (таб из сессии): вместо списка — «Не
/// подключено» и «Подключить Enter».
pub fn offline(s: &AppState, t: &Tab) -> bool {
    failed(t) && link(s, t).is_some_and(|l| l != Link::Up)
}

/// Связи нет, а листинг на экране — с последнего чтения: панель приглушена
/// (`screen-sftplost`), в заголовке «из кэша».
pub fn stale(s: &AppState, t: &Tab) -> bool {
    !failed(t) && matches!(link(s, t), Some(Link::Lost { .. } | Link::Off))
}

/// Команда «Подключить» таба без связи: папка таба в его панели.
pub fn connect_cmd(t: &Tab, side: Side) -> Option<Command> {
    let p = profile_of(t)?;
    let path = t.loc.path.to_string_lossy().replace('\\', "/");
    Some(
        Command::new("sftp.open")
            .arg("profile", p)
            .arg("path", if path.is_empty() { "/".into() } else { path })
            .arg("side", crate::side_arg(side)),
    )
}

/// Справа в заголовке панели `sftp://`: «N объектов · 8 мс», без связи — «N объектов ·
/// из кэша». Не `sftp://` или без числа — `count` как есть.
pub fn head_count(s: &AppState, t: &Tab, count: String) -> String {
    if count.is_empty() {
        return count;
    }
    let i18n = urfin_i18n::current();
    let mut a = FluentArgs::new();
    if stale(s, t) {
        a.set("count", count);
        return i18n.fmt("net-head-cached", &a);
    }
    match ping(s, t) {
        Some(ms) => {
            a.set("count", count);
            a.set("ms", ms);
            i18n.fmt("net-head-ping", &a)
        }
        None => count,
    }
}

/// Статус панели `sftp://` одной группой (№92): «admin@srv1.corp.local · ssh-ed25519 ·
/// ключ из агента (Pageant)»; без сеанса — `user@host` профиля.
pub fn panel_status(s: &AppState, t: &Tab) -> Option<String> {
    let p = profile_of(t)?;
    let st = net(s)?;
    let i18n = urfin_i18n::current();
    let Some(l) = st.live(&p) else {
        return Some(st.profile(&p).map_or(p, live::user_host));
    };
    let mut parts = vec![l.user_host.clone()];
    if !l.algo.is_empty() {
        parts.push(l.algo.clone());
    }
    if !l.login.is_empty() {
        let login: Vec<String> = l.login.iter().map(|x| x.text(&i18n)).collect();
        parts.push(login.join(" + "));
    }
    Some(parts.join(" · "))
}

/// Строка обрыва внизу панели (`screen-sftplost`): на какой стороне и что показать.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LostRow {
    pub side: Side,
    pub profile: String,
    /// `Link::Lost` или `Link::Off`.
    pub link: Link,
}

/// Строка обрыва: активная панель, затем другая — та, что показывает папку профиля без
/// связи. Вопрос подключения и «Новое подключение» важнее (их строки — свои).
pub fn lost_row(s: &AppState) -> Option<LostRow> {
    if net(s).is_some_and(|n| n.ask().is_some() || n.form.is_some()) {
        return None;
    }
    [s.active, s.active.other()].into_iter().find_map(|side| {
        let pn = s.panel(side);
        let t = pn.tab();
        if !pn.visible || !stale(s, t) {
            return None;
        }
        Some(LostRow {
            side,
            profile: profile_of(t)?,
            link: link(s, t)?,
        })
    })
}

/// Текст строки обрыва: заголовок и пояснение. `now` — для отсчёта.
pub fn lost_text(r: &LostRow, now: std::time::Instant) -> (String, String) {
    let i18n = urfin_i18n::current();
    let mut a = FluentArgs::new();
    a.set("profile", r.profile.clone());
    match r.link {
        Link::Lost {
            attempt,
            tries,
            retry_at,
        } => {
            a.set("attempt", attempt);
            a.set("tries", tries);
            let left = retry_at.map(|at| at.saturating_duration_since(now));
            let note = match left {
                Some(d) if !d.is_zero() => {
                    let secs = d.as_millis().div_ceil(1000);
                    a.set("s", u64::try_from(secs).unwrap_or(u64::MAX));
                    i18n.fmt("net-lost-wait", &a)
                }
                _ => i18n.fmt("net-lost-trying", &a),
            };
            (i18n.fmt("net-lost", &a), note)
        }
        _ => (i18n.fmt("net-off", &a), i18n.get("net-off-note")),
    }
}

/// Кнопки строки обрыва: подпись, клавиша, команда. Обрыв — «Переподключить сейчас» и
/// «Отключиться»; без сеанса — «Переподключить».
pub fn lost_buttons(r: &LostRow) -> Vec<(String, &'static str, Command)> {
    let i18n = urfin_i18n::current();
    let reconnect = Command::new("ssh.reconnect").arg("profile", r.profile.clone());
    match r.link {
        Link::Lost { .. } => vec![
            (i18n.get("net-lost-now"), "Enter", reconnect),
            (
                i18n.get("net-lost-disconnect"),
                "",
                Command::new("ssh.disconnect").arg("profile", r.profile.clone()),
            ),
        ],
        _ => vec![(i18n.get("net-off-reconnect"), "Enter", reconnect)],
    }
}

/// Клавиши строки обрыва (`screen-sftplost`): Tab — между кнопками.
pub fn lost_keys(r: &LostRow) -> Vec<(String, String)> {
    if lost_buttons(r).len() < 2 {
        return Vec::new();
    }
    vec![(
        "Tab".to_owned(),
        urfin_i18n::current().get("net-key-next-button"),
    )]
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
    st.list()
        .iter()
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
        Auth::Agent => "net-via-agent",
        Auth::Key => "net-via-key",
        Auth::Password => "net-via-password",
        Auth::Command => "net-via-command",
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
        .map(|n| {
            n.list()
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
    let file = st.and_then(|n| n.connections.as_ref()).map_or_else(
        || r"%APPDATA%\Urfin\connections.toml".to_owned(),
        |f| f.display().to_string(),
    );
    let list = st.map_or(&[][..], NetModel::list);
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
        let mut st = NetModel::default();
        let mut p = Profile::new("srv1", "srv1.corp.local");
        p.user = "admin".into();
        p.source = ProfileSource::Config;
        st.profiles = Some(vec![p, Profile::new("own", "h")]);
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

    /// Состояние со справа табом `sftp://srv1/var/www` и `n` записями листинга (0 — не
    /// прочитан: таб из сессии).
    fn sftp_state(n: usize) -> AppState {
        use urfin_core::{Entry, EntryKind, Gen, Listing, ListingStatus, Location, Panel, TabId};
        let mut loc = Location::local(r"C:\w");
        let left = Tab::new(TabId(1), loc.clone());
        loc.backend = live::SFTP_SCHEME.into();
        loc.authority = Some("srv1".into());
        loc.path = "/var/www".into();
        let mut right = Tab::new(TabId(2), loc);
        if n == 0 {
            right.listing.status = ListingStatus::Failed("not connected".into());
        } else {
            let entries: Vec<Entry> = (0..n)
                .map(|i| Entry {
                    name: format!("f{i}").into(),
                    size: 1,
                    mtime: 0,
                    ctime: 0,
                    atime: 0,
                    attrs: urfin_core::Attrs(0),
                    kind: EntryKind::File,
                })
                .collect();
            let order = (0..u32::try_from(n).unwrap()).collect();
            right.listing = Listing::with_entries(entries, order, Gen::default());
        }
        let mut s = AppState::new(Panel::new(left), Panel::new(right));
        s.active = Side::Right;
        let mut st = NetModel::default();
        let mut p = Profile::new("srv1", "srv1.corp.local");
        p.user = "admin".into();
        st.profiles = Some(vec![p]);
        s.ext.insert(st);
        s
    }

    fn set_live(s: &mut AppState, state: Option<LiveState>) {
        let st = s.ext.get_mut::<NetModel>().unwrap();
        st.live.clear();
        if let Some(state) = state {
            st.live.push(live::Live {
                algo: "ssh-ed25519".into(),
                login: vec![live::Login::Agent { pageant: true }],
                ..live::Live::new("srv1", "admin@srv1.corp.local", state, Side::Right)
            });
        }
    }

    #[test]
    fn sftp_link_states() {
        let mut s = sftp_state(3);
        assert_eq!(link(&s, s.panel(Side::Left).tab()), None);
        let t = s.panel(Side::Right).tab();
        assert_eq!(link(&s, t), Some(Link::Off));
        assert!(stale(&s, t) && !offline(&s, t));
        set_live(&mut s, Some(LiveState::Connected));
        let t = s.panel(Side::Right).tab();
        assert_eq!(link(&s, t), Some(Link::Up));
        assert!(!stale(&s, t));
        assert_eq!(lost_row(&s), None);
        // Статус одной группой (№92).
        let i18n = urfin_i18n::current();
        assert_eq!(
            panel_status(&s, t).unwrap(),
            format!(
                "admin@srv1.corp.local · ssh-ed25519 · {}",
                i18n.get("net-login-pageant")
            )
        );
        assert_eq!(panel_status(&s, s.panel(Side::Left).tab()), None);
    }

    #[test]
    #[allow(
        clippy::many_single_char_names,
        reason = "состояние, таб, строка, аргументы"
    )]
    fn lost_row_counts_down_and_keeps_listing() {
        let mut s = sftp_state(3);
        let now = std::time::Instant::now();
        set_live(
            &mut s,
            Some(LiveState::Lost {
                attempt: 2,
                tries: 5,
                retry_at: Some(now + std::time::Duration::from_millis(2500)),
            }),
        );
        let t = s.panel(Side::Right).tab();
        assert!(stale(&s, t));
        let i18n = urfin_i18n::current();
        let mut a = FluentArgs::new();
        a.set("count", "3");
        assert_eq!(
            head_count(&s, t, "3".into()),
            i18n.fmt("net-head-cached", &a)
        );
        let r = lost_row(&s).unwrap();
        assert_eq!((r.side, r.profile.as_str()), (Side::Right, "srv1"));
        let mut a = FluentArgs::new();
        a.set("s", 3);
        a.set("attempt", 2);
        a.set("tries", 5);
        assert_eq!(lost_text(&r, now).1, i18n.fmt("net-lost-wait", &a));
        assert_eq!(
            lost_text(&r, now + std::time::Duration::from_secs(3)).1,
            i18n.fmt("net-lost-trying", &a)
        );
        let b = lost_buttons(&r);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].2.id.as_str(), "ssh.reconnect");
        assert_eq!(b[1].2.id.as_str(), "ssh.disconnect");
        assert_eq!(lost_keys(&r).len(), 1);
        // Попытки исчерпаны — одна кнопка «Переподключить».
        set_live(&mut s, Some(LiveState::Disconnected));
        let r = lost_row(&s).unwrap();
        assert_eq!(lost_buttons(&r).len(), 1);
        assert_eq!(lost_keys(&r).len(), 0);
        // Вопрос подключения важнее строки обрыва.
        s.ext
            .get_mut::<NetModel>()
            .unwrap()
            .asks
            .push_back(live::Ask::new(
                "srv1",
                "srv1.corp.local",
                "",
                AskKind::HostKey {
                    algo: String::new(),
                    fingerprint: String::new(),
                },
            ));
        assert_eq!(lost_row(&s), None);
    }

    #[test]
    fn session_tab_without_connection_offers_connect() {
        let mut s = sftp_state(0);
        let t = s.panel(Side::Right).tab();
        assert!(offline(&s, t) && !stale(&s, t));
        assert_eq!(
            lost_row(&s),
            None,
            "без листинга — «Не подключено», не строка"
        );
        let c = connect_cmd(t, Side::Right).unwrap();
        assert_eq!(c.id.as_str(), "sftp.open");
        assert_eq!(c.args.str("profile"), Some("srv1"));
        assert_eq!(c.args.str("path"), Some("/var/www"));
        assert_eq!(c.args.str("side"), Some("right"));
        assert_eq!(
            panel_status(&s, t).as_deref(),
            Some("admin@srv1.corp.local")
        );
        set_live(&mut s, Some(LiveState::Connected));
        assert!(!offline(&s, s.panel(Side::Right).tab()));
    }
}
