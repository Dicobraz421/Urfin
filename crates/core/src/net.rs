//! Модель подключений SSH/SFTP для UI (F03-05a, [решение 2026-11-02 «net-model-core»]):
//! профили, история хостов, сеансы, вопросы подключения (ключ сервера, пароль, код),
//! строка «Новое подключение», пинг. Только данные — без сокетов, крипто и диска.
//! Лежит в `AppState.ext` как [`NetModel`]; наполняет `urfin-net` (handler `ssh`/`sftp`
//! и его `Msg`), UI только читает. Каналы ответов и управления сеансом — у `urfin-net`.
//!
//! [решение 2026-11-02 «net-model-core»]: ../../../docs/decisions/2026-11-02-net-model-core.md

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use toml::value::Datetime;
use urfin_i18n::{FluentArgs, I18n};

use crate::{Location, Side};

/// Схема адресов SFTP: `sftp://<профиль>/путь`.
pub const SFTP_SCHEME: &str = "sftp";

/// Порт SSH по умолчанию.
pub const DEFAULT_PORT: u16 = 22;

/// Способ входа.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Auth {
    /// Агент OpenSSH или Pageant, затем ключи `[ssh].default_keys`.
    #[default]
    Agent,
    Key,
    Password,
    /// Пароль — командой менеджера паролей.
    Command,
}

/// Откуда профиль. Порядок вариантов — приоритет при совпадении имён (первый сильнее).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProfileSource {
    #[default]
    Urfin,
    Config,
    Putty,
    /// Разовый профиль из ввода `sftp://user@host:port/…` (F03 §2.6): в файл не пишется.
    History,
}

impl ProfileSource {
    pub fn label(self, i18n: &I18n) -> String {
        i18n.get(match self {
            ProfileSource::Urfin => "net-source-urfin",
            ProfileSource::Config => "net-source-config",
            ProfileSource::Putty => "net-source-putty",
            ProfileSource::History => "net-source-history",
        })
    }
}

/// Подпись в свойствах импортированного профиля.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    /// Директива `ssh_config` пропущена: «не поддерживается: Match (строка N)».
    Unsupported {
        directive: String,
        file: PathBuf,
        line: u32,
    },
    /// `.ppk` из `PuTTY`: ключ — через Pageant.
    KeyViaPageant,
}

impl Note {
    pub fn text(&self, i18n: &I18n) -> String {
        match self {
            Note::Unsupported {
                directive, line, ..
            } => {
                let mut a = FluentArgs::new();
                a.set("directive", directive.clone());
                a.set("line", line.to_string());
                i18n.fmt("net-note-unsupported", &a)
            }
            Note::KeyViaPageant => i18n.get("net-note-pageant"),
        }
    }
}

/// Профиль: запись `[[profile]]` формата `connections.toml` v1 и поля, которые знает
/// только память (источник, keep-alive из `ssh_config`, подписи). Секретов здесь нет.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    /// Authority в `sftp://<имя>/…`.
    pub name: String,
    pub host: String,
    pub port: u16,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub user: String,
    pub auth: Auth,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub key: String,
    /// `ProxyJump`: имя профиля или `user@host:port`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub via: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub start_dir: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bookmarks: Vec<String>,
    /// Пусто — из `[ssh].password_cmd`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub password_cmd: String,
    /// Подстановка `{id}` команды пароля.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub password_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used: Option<Datetime>,
    #[serde(skip)]
    pub source: ProfileSource,
    /// `ServerAliveInterval` из `ssh_config`; иначе — `[ssh].keepalive_s`.
    #[serde(skip)]
    pub keepalive_s: Option<u32>,
    #[serde(skip)]
    pub notes: Vec<Note>,
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            name: String::new(),
            host: String::new(),
            port: DEFAULT_PORT,
            user: String::new(),
            auth: Auth::Agent,
            key: String::new(),
            via: String::new(),
            start_dir: String::new(),
            bookmarks: Vec::new(),
            password_cmd: String::new(),
            password_id: String::new(),
            last_used: None,
            source: ProfileSource::Urfin,
            keepalive_s: None,
            notes: Vec::new(),
        }
    }
}

impl Profile {
    pub fn new(name: impl Into<String>, host: impl Into<String>) -> Profile {
        Profile {
            name: name.into(),
            host: host.into(),
            ..Profile::default()
        }
    }
}

/// Шаблон команды менеджера паролей (`[ssh].password_manager`); пусто — не выбран.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PasswordManager {
    #[default]
    #[serde(rename = "")]
    None,
    #[serde(rename = "bearpass")]
    BearPass,
    #[serde(rename = "passwork")]
    Passwork,
    #[serde(rename = "1password")]
    OnePassword,
    #[serde(rename = "bitwarden")]
    Bitwarden,
    #[serde(rename = "keepassxc")]
    KeePassXc,
    #[serde(rename = "custom")]
    Custom,
}

impl PasswordManager {
    /// Шаблон команды менеджера. `BearPass` и Пассворк — сверить на стенде
    /// (`docs/manual-tests/F03-03.md`).
    pub fn template(self) -> Option<&'static str> {
        Some(match self {
            PasswordManager::BearPass => "bearpass get {id} --field=password",
            PasswordManager::Passwork => "passwork-cli get --password-id {id}",
            PasswordManager::OnePassword => r#"op read "op://Servers/{host}/password""#,
            PasswordManager::Bitwarden => r#"bw get password "{host}""#,
            PasswordManager::KeePassXc => "keepassxc-cli show -s -a Password db.kdbx {host}",
            PasswordManager::None | PasswordManager::Custom => return None,
        })
    }
}

/// Записей в истории хостов максимум; старые вытесняются.
pub const HISTORY_MAX: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub host: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user: String,
    pub time: Datetime,
}

/// История хостов (`state/ssh/history.toml`): новые — первыми; пара хост + пользователь
/// — одна запись. Чтение и запись файла — у `urfin-net`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    #[serde(default, rename = "host", skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<HistoryEntry>,
}

impl History {
    /// Подключение к `host` под `user` в `time`: запись — в начало.
    pub fn touch(&mut self, host: &str, user: &str, time: Datetime) {
        self.entries
            .retain(|e| !(e.host.eq_ignore_ascii_case(host) && e.user == user));
        self.entries.insert(
            0,
            HistoryEntry {
                host: host.to_owned(),
                user: user.to_owned(),
                time,
            },
        );
        self.entries.truncate(HISTORY_MAX);
    }
}

/// Фаза подключения узла — «k из 3» в статусе.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Tcp,
    HostKey,
    Auth,
}

impl Phase {
    /// Номер фазы для «k из 3».
    pub fn no(self) -> u8 {
        match self {
            Phase::Tcp => 1,
            Phase::HostKey => 2,
            Phase::Auth => 3,
        }
    }
}

/// Что спрашивает строка ввода.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptKind {
    Password,
    /// Фраза-пароль ключа.
    Passphrase(PathBuf),
    /// Запрос keyboard-interactive (код 2FA и т. п.).
    Interactive {
        name: String,
        instructions: String,
    },
}

/// Что с сеансом профиля.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveState {
    /// Идут фазы TCP → ключ сервера → вход.
    Connecting,
    Connected,
    /// Обрыв: попытка `attempt` из `tries`.
    Lost {
        attempt: u32,
        tries: u32,
    },
    Disconnected,
    /// Не подключились: текст для строки ошибки.
    Failed(String),
}

/// Сеанс профиля для UI. Канал управления сеансом — у `urfin-net`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Live {
    pub profile: String,
    /// `admin@srv1.corp.local` — подсказка пути и статус (№87).
    pub user_host: String,
    pub state: LiveState,
    /// Фаза последнего узла цепочки — «k из 3» в статусе.
    pub phase: Option<Phase>,
    /// Где открыть папку после подключения.
    pub side: Side,
    pub path: String,
}

impl Live {
    /// Сеанс в корне сервера, без фазы.
    pub fn new(profile: &str, user_host: &str, state: LiveState, side: Side) -> Live {
        Live {
            profile: profile.to_owned(),
            user_host: user_host.to_owned(),
            state,
            phase: None,
            side,
            path: "/".into(),
        }
    }
}

/// Вопрос подключения: строка внизу панели. Ответ — командой `ssh.answer`; канал ответа
/// — у `urfin-net` по [`Ask::id`] (нет канала — «Отмена»).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AskKind {
    /// Неизвестный ключ (`screen-sshhostkey`).
    HostKey { algo: String, fingerprint: String },
    /// Ключ сменился (`screen-sshhostchanged`): было (`file:line`) и сейчас.
    HostChanged {
        algo: String,
        file: PathBuf,
        line: usize,
        old: String,
        fingerprint: String,
    },
    /// Пароль, фраза-пароль, код (`screen-sshpass`).
    Secret {
        kind: PromptKind,
        text: String,
        echo: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    /// Номер вопроса: новый — новые поля строки.
    pub id: u64,
    pub profile: String,
    /// Узел цепочки: `host` или `[host]:port`.
    pub host: String,
    /// `admin@srv1.corp.local`.
    pub user_host: String,
    pub kind: AskKind,
}

static ASK_SEQ: AtomicU64 = AtomicU64::new(1);

impl Ask {
    /// Новый вопрос (номер — следующий).
    pub fn new(profile: &str, host: &str, user_host: &str, kind: AskKind) -> Ask {
        Ask {
            id: ASK_SEQ.fetch_add(1, Ordering::Relaxed),
            profile: profile.to_owned(),
            host: host.to_owned(),
            user_host: user_host.to_owned(),
            kind,
        }
    }

    /// Имя хоста для подтверждения замены ключа: без `[]:port`.
    pub fn plain_host(&self) -> &str {
        self.host
            .strip_prefix('[')
            .and_then(|r| r.split_once(']'))
            .map_or(self.host.as_str(), |(h, _)| h)
    }
}

/// Строка «Новое подключение» (`screen-sshnew`): начальные значения полей.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Form {
    /// Номер открытия: новый — новые поля.
    pub seq: u64,
    pub host: String,
    pub user: String,
    pub port: String,
    pub via: String,
    pub auth: Auth,
    pub save: bool,
    /// Правка профиля с этим именем (F4), иначе новый.
    pub edit: Option<String>,
    pub side: Side,
}

/// Подпись способа входа.
pub fn auth_key(a: Auth) -> &'static str {
    match a {
        Auth::Agent => "net-auth-agent",
        Auth::Key => "net-auth-key",
        Auth::Password => "net-auth-password",
        Auth::Command => "net-auth-command",
    }
}

/// Значение аргумента `auth` команды `ssh.connect`.
pub fn auth_id(a: Auth) -> &'static str {
    match a {
        Auth::Agent => "agent",
        Auth::Key => "key",
        Auth::Password => "password",
        Auth::Command => "command",
    }
}

pub fn parse_auth(s: &str) -> Option<Auth> {
    [Auth::Agent, Auth::Key, Auth::Password, Auth::Command]
        .into_iter()
        .find(|a| auth_id(*a).eq_ignore_ascii_case(s))
}

/// Вариант автодополнения «Хоста»: значение и источник справа.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostHint {
    pub host: String,
    pub source: HintSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HintSource {
    /// `~/.ssh/config` · `Host <имя>`.
    Config(String),
    /// История · когда (секунды Unix).
    History(Option<i64>),
    KnownHosts,
    /// Свой профиль.
    Profile(String),
}

/// `user@host` профиля (пользователь по умолчанию — из ОС).
pub fn user_host(p: &Profile) -> String {
    if p.user.is_empty() {
        p.host.clone()
    } else {
        format!("{}@{}", p.user, p.host)
    }
}

/// Профиль таба `sftp://<профиль>/…` (без контейнера и пустого authority — `None`).
pub fn sftp_profile(loc: &Location) -> Option<String> {
    if loc.backend != SFTP_SCHEME || loc.outer.is_some() {
        return None;
    }
    loc.authority
        .as_deref()
        .filter(|a| !a.is_empty())
        .map(str::to_owned)
}

/// Пинг профиля для заголовка панели (F03 §2.3): RTT последнего листинга или keep-alive.
/// Реализует backend `sftp` в `urfin-net`; чтение — из памяти, без I/O.
pub trait Ping: Send + Sync {
    fn rtt(&self, profile: &str) -> Option<Duration>;
}

/// Модель подключений в `AppState.ext` (F03-05a). Мутации — в handler `ssh`/`sftp` и
/// его `Msg` (и фикстурах сцен `--screens`); UI читает.
#[derive(Default)]
pub struct NetModel {
    /// Профили: свои, импортированные; `None` — ещё не читались.
    pub profiles: Option<Vec<Profile>>,
    pub history: Option<History>,
    /// Хосты из `~/.ssh/known_hosts` — автодополнение «Хоста».
    pub known: Vec<String>,
    /// Ключи серверов профилей из `known_hosts` (имя профиля в нижнем регистре →
    /// алгоритм, отпечаток) — «Свойства» в «Подключениях».
    pub host_keys: HashMap<String, (String, String)>,
    /// Разовые профили этого запуска (`sftp://user@host`, строка без «Сохранить»).
    pub adhoc: Vec<Profile>,
    /// Сеансы профилей.
    pub live: Vec<Live>,
    /// Вопросы подключения по очереди; показан первый.
    pub asks: VecDeque<Ask>,
    /// Открытая строка «Новое подключение».
    pub form: Option<Form>,
    /// Растёт при каждой правке — ключ кэша кадра UI.
    pub rev: u64,
    /// `connections.toml` — статус «Подключений»; `None` — без диска.
    pub connections: Option<PathBuf>,
    /// Пинг панелей `sftp://`; `None` — без backend'а (тесты UI).
    pub ping: Option<Arc<dyn Ping>>,
}

impl fmt::Debug for NetModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetModel")
            .field("profiles", &self.profiles.as_ref().map(Vec::len))
            .field("live", &self.live)
            .field("asks", &self.asks.len())
            .field("form", &self.form)
            .field("rev", &self.rev)
            .finish_non_exhaustive()
    }
}

impl NetModel {
    /// Профили (не прочитаны — пусто).
    pub fn list(&self) -> &[Profile] {
        self.profiles.as_deref().unwrap_or_default()
    }

    /// Профиль по имени: свои, импортированные, разовые этого запуска.
    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.list()
            .iter()
            .chain(&self.adhoc)
            .find(|p| p.name.eq_ignore_ascii_case(name))
    }

    /// Сеанс профиля.
    pub fn live(&self, profile: &str) -> Option<&Live> {
        self.live
            .iter()
            .find(|l| l.profile.eq_ignore_ascii_case(profile))
    }

    pub fn live_mut(&mut self, profile: &str) -> Option<&mut Live> {
        self.live
            .iter_mut()
            .find(|l| l.profile.eq_ignore_ascii_case(profile))
    }

    /// Вопрос, который показан (первый в очереди).
    pub fn ask(&self) -> Option<&Ask> {
        self.asks.front()
    }

    /// Подключённых профилей (статус «SSH: N подключений»).
    pub fn connected(&self) -> usize {
        self.live
            .iter()
            .filter(|l| l.state == LiveState::Connected)
            .count()
    }

    /// Пинг профиля: RTT последнего листинга или keep-alive.
    pub fn rtt(&self, profile: &str) -> Option<Duration> {
        self.ping.as_ref()?.rtt(profile)
    }

    /// Варианты для набранного `text`: начинаются с него (без регистра), без повторов;
    /// порядок — config, история, `known_hosts`, свои профили (`screen-sshnew`).
    pub fn host_hints(&self, text: &str) -> Vec<HostHint> {
        let t = text.trim().to_lowercase();
        let mut out: Vec<HostHint> = Vec::new();
        let mut push = |host: &str, source: HintSource| {
            if host.to_lowercase().starts_with(&t)
                && !out.iter().any(|h| h.host.eq_ignore_ascii_case(host))
            {
                out.push(HostHint {
                    host: host.to_owned(),
                    source,
                });
            }
        };
        let list = self.list();
        for p in list.iter().filter(|p| p.source == ProfileSource::Config) {
            push(&p.host, HintSource::Config(p.name.clone()));
        }
        for e in self.history.iter().flat_map(|h| &h.entries) {
            push(&e.host, HintSource::History(unix(&e.time)));
        }
        for h in &self.known {
            push(h, HintSource::KnownHosts);
        }
        for p in list.iter().filter(|p| p.source != ProfileSource::Config) {
            push(&p.host, HintSource::Profile(p.name.clone()));
        }
        out
    }
}

/// Секунды Unix → `Datetime` UTC.
pub fn datetime(secs: i64) -> Datetime {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
    .parse()
    .expect("valid RFC 3339")
}

/// `Datetime` → секунды Unix (без даты — `None`; без зоны — как UTC).
pub fn unix(t: &Datetime) -> Option<i64> {
    let d = t.date?;
    let days = days_from_civil(i64::from(d.year), u32::from(d.month), u32::from(d.day));
    let (h, mi, s) = t.time.map_or((0, 0, 0), |t| {
        (
            i64::from(t.hour),
            i64::from(t.minute),
            i64::from(t.second.unwrap_or(0)),
        )
    });
    let off = match t.offset {
        Some(toml::value::Offset::Custom { minutes }) => i64::from(minutes) * 60,
        _ => 0,
    };
    Some(days * 86_400 + h * 3600 + mi * 60 + s - off)
}

/// Дни от 1970-01-01 → (год, месяц, день) (H. Hinnant, `civil_from_days`).
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datetime_roundtrip() {
        for secs in [0, 1_761_210_840, 951_782_400, 4_102_444_799] {
            assert_eq!(unix(&datetime(secs)), Some(secs), "{secs}");
        }
        assert_eq!(datetime(1_761_210_840).to_string(), "2025-10-23T09:14:00Z");
        let t: Datetime = "2026-10-23T12:14:00+03:00".parse().unwrap();
        assert_eq!(unix(&t), unix(&"2026-10-23T09:14:00Z".parse().unwrap()));
    }

    #[test]
    fn auth_ids_roundtrip() {
        for a in [Auth::Agent, Auth::Key, Auth::Password, Auth::Command] {
            assert_eq!(parse_auth(auth_id(a)), Some(a));
        }
        assert_eq!(parse_auth("nope"), None);
    }

    #[test]
    fn history_touch_moves_to_front_and_caps() {
        let t = |s: &str| -> Datetime { s.parse().unwrap() };
        let mut h = History::default();
        h.touch("a", "root", t("2026-10-23T09:00:00Z"));
        h.touch("b", "", t("2026-10-23T09:01:00Z"));
        h.touch("A", "root", t("2026-10-23T09:02:00Z"));
        let hosts: Vec<_> = h.entries.iter().map(|e| e.host.as_str()).collect();
        assert_eq!(hosts, ["A", "b"]);
        for i in 0..HISTORY_MAX + 5 {
            h.touch(&format!("h{i}"), "", t("2026-10-23T10:00:00Z"));
        }
        assert_eq!(h.entries.len(), HISTORY_MAX);
    }

    fn model() -> NetModel {
        let mut m = NetModel::default();
        let mut cfg = Profile::new("srv1", "srv1.corp.local");
        cfg.source = ProfileSource::Config;
        m.profiles = Some(vec![cfg, Profile::new("web", "srv1-web.corp.local")]);
        let mut h = History::default();
        h.touch("srv1-stage.corp.local", "admin", datetime(1_761_210_840));
        h.touch("srv1.corp.local", "admin", datetime(1_761_210_840));
        m.history = Some(h);
        m.known = vec![
            "srv1-db.corp.local".into(),
            "srv10.corp.local".into(),
            "db".into(),
        ];
        m
    }

    #[test]
    fn hints_by_source_without_repeats() {
        let m = model();
        let h = m.host_hints("SRV1");
        let hosts: Vec<&str> = h.iter().map(|h| h.host.as_str()).collect();
        assert_eq!(
            hosts,
            [
                "srv1.corp.local",
                "srv1-stage.corp.local",
                "srv1-db.corp.local",
                "srv10.corp.local",
                "srv1-web.corp.local"
            ]
        );
        assert_eq!(h[0].source, HintSource::Config("srv1".into()));
        assert!(matches!(h[1].source, HintSource::History(Some(_))));
        assert_eq!(h[2].source, HintSource::KnownHosts);
        assert_eq!(h[4].source, HintSource::Profile("web".into()));
        assert_eq!(m.host_hints("").len(), 6);
    }

    #[test]
    fn lookup_and_counts() {
        let mut m = model();
        m.adhoc.push(Profile::new("once", "h"));
        assert!(m.profile("SRV1").is_some() && m.profile("once").is_some());
        m.live
            .push(Live::new("srv1", "a@h", LiveState::Connected, Side::Left));
        m.live
            .push(Live::new("web", "h", LiveState::Connecting, Side::Right));
        assert_eq!(m.connected(), 1);
        assert_eq!(m.live("WEB").map(|l| l.side), Some(Side::Right));
        assert_eq!(m.rtt("srv1"), None);
    }

    #[test]
    fn plain_host_strips_port() {
        let kind = AskKind::HostKey {
            algo: "ssh-ed25519".into(),
            fingerprint: "SHA256:a".into(),
        };
        let a = Ask::new("srv1", "[srv1.corp.local]:2222", "", kind.clone());
        let b = Ask::new("srv1", "srv1.corp.local", "", kind);
        assert_eq!(a.plain_host(), "srv1.corp.local");
        assert_eq!(b.plain_host(), "srv1.corp.local");
        assert!(b.id > a.id);
    }

    #[test]
    fn sftp_profile_of_location() {
        let mut loc = Location::local(std::env::temp_dir());
        assert_eq!(sftp_profile(&loc), None);
        loc.backend = SFTP_SCHEME.into();
        loc.authority = Some("srv1".into());
        assert_eq!(sftp_profile(&loc).as_deref(), Some("srv1"));
        loc.authority = Some("".into());
        assert_eq!(sftp_profile(&loc), None);
    }

    #[test]
    fn templates_only_for_known_managers() {
        assert!(PasswordManager::BearPass.template().is_some());
        assert_eq!(PasswordManager::None.template(), None);
        assert_eq!(PasswordManager::Custom.template(), None);
    }
}
