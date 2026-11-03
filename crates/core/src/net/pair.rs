//! SSH-терминал и пара «панель + терминал» для UI (F03 §3.1, §3.2, F03-09): сеансы
//! `ssh.exe` профилей и пара — панель `sftp://` сверху, терминал того же профиля под ней.
//! Только данные; сеансы терминала — у `urfin-term`, мост `ssh.exe` — у `urfin-net`.

use std::path::PathBuf;

use urfin_i18n::{FluentArgs, I18n};

use crate::{AppState, Focus, Side};

/// Чем входит `ssh.exe` — статус пары («ssh.exe · ключ ed25519 из агента», №92).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TermLogin {
    /// Ключ из агента: агент Урфина (ключ менеджера паролей, `algo` — `ed25519`) или
    /// системный агент (`algo` неизвестен).
    Agent { algo: Option<String> },
    /// Ключ из файла (`-i`).
    Key(PathBuf),
    /// Пароль через `SSH_ASKPASS` из памяти сеанса Урфина.
    Password,
    /// Мост недоступен (OpenSSH < 8.9): пароль вводится в самом терминале.
    InTerminal,
}

impl TermLogin {
    pub fn text(&self, i18n: &I18n) -> String {
        match self {
            TermLogin::Agent { algo: Some(algo) } => {
                let mut a = FluentArgs::new();
                a.set("algo", algo.clone());
                i18n.fmt("net-term-login-agent-key", &a)
            }
            TermLogin::Agent { algo: None } => i18n.get("net-login-agent"),
            TermLogin::Key(path) => {
                let mut a = FluentArgs::new();
                let file = path.file_name().unwrap_or(path.as_os_str());
                a.set("file", file.to_string_lossy().into_owned());
                i18n.fmt("net-login-key", &a)
            }
            TermLogin::Password => i18n.get("net-term-login-askpass"),
            TermLogin::InTerminal => i18n.get("net-term-login-terminal"),
        }
    }
}

/// Сеанс терминала профиля: `id` — сеанс `urfin-term`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TermLink {
    pub id: u32,
    pub profile: String,
    /// Номер сеанса профиля с 1 — меню «ssh srv1 ▾».
    pub n: u32,
    pub login: TermLogin,
}

/// Папка терминала для панели (OSC 7, F03 §3.2).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum PairCwd {
    /// OSC 7 ещё не было.
    #[default]
    Wait,
    /// Сервер OSC 7 не шлёт — «папка: сервер не сообщает».
    Silent,
    /// Последняя папка оболочки (путь POSIX).
    At(String),
}

/// Пара «панель + терминал» (`screen-sshpair`).
#[derive(Clone, Debug, PartialEq)]
pub struct Pair {
    pub profile: String,
    /// Половина окна: сверху панель `sftp://` профиля, под ней терминал.
    pub side: Side,
    /// Сеанс терминала пары; `None` — ещё запускается (ждёт подключения панели).
    pub term: Option<u32>,
    /// Фокус у терминала (Ctrl+Tab, `ssh.pair_focus`).
    pub focus_term: bool,
    /// Активный сеанс терминала до перехода в пару: вернётся с фокусом в панель.
    pub back: Option<u32>,
    pub cwd: PairCwd,
    /// Хост первого OSC 7: дальше OSC 7 с другим хостом (вложенный `ssh`) панель не
    /// двигает.
    pub host: Option<String>,
    /// Доля высоты панели (сплиттер), по умолчанию 45/55.
    pub split: f32,
}

/// Доля панели в паре по умолчанию (F03 §3.2).
pub const PAIR_SPLIT: f32 = 0.45;

impl Pair {
    pub fn new(profile: &str, side: Side) -> Pair {
        Pair {
            profile: profile.to_owned(),
            side,
            term: None,
            focus_term: false,
            back: None,
            cwd: PairCwd::Wait,
            host: None,
            split: PAIR_SPLIT,
        }
    }
}

/// Контекст keymap `in_sshpair` (№88): пара есть и фокус у неё — у её терминала или у её
/// панели.
pub fn in_sshpair(st: &AppState) -> bool {
    let Some(p) = st
        .ext
        .get::<super::NetModel>()
        .and_then(|m| m.pair.as_ref())
    else {
        return false;
    };
    match st.focus {
        Focus::Terminal => p.focus_term,
        Focus::Panel => st.active == p.side,
        _ => false,
    }
}

/// Фокус у терминала пары: показ панелей его не уводит в панель.
pub fn pair_term_focused(st: &AppState) -> bool {
    st.focus == Focus::Terminal
        && st
            .ext
            .get::<super::NetModel>()
            .and_then(|m| m.pair.as_ref())
            .is_some_and(|p| p.focus_term)
}

/// Путь POSIX без `.`, `..`, двойных и конечного `/` (OSC 7 → панель).
pub fn posix_normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for c in path.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    format!("/{}", parts.join("/"))
}

/// Хост OSC 7 — тот же сервер: пусто, `localhost`, хост или имя профиля (короткое имя
/// до первой точки тоже).
pub fn same_host(osc: &str, host: &str, profile: &str) -> bool {
    let short = |s: &str| s.split('.').next().unwrap_or(s).to_ascii_lowercase();
    let o = osc.to_ascii_lowercase();
    o.is_empty()
        || o == "localhost"
        || o == host.to_ascii_lowercase()
        || o == profile.to_ascii_lowercase()
        || (!o.chars().all(|c| c.is_ascii_digit() || c == '.')
            && (short(&o) == short(host) || short(&o) == short(profile)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize() {
        assert_eq!(posix_normalize("/var/www/app/"), "/var/www/app");
        assert_eq!(posix_normalize("/var//www/./x/../app"), "/var/www/app");
        assert_eq!(posix_normalize("/"), "/");
        assert_eq!(posix_normalize("/.."), "/");
        assert_eq!(posix_normalize(""), "/");
    }

    #[test]
    fn hosts() {
        assert!(same_host("", "10.0.0.1", "srv1"));
        assert!(same_host("srv1", "srv1.corp.local", "web"));
        assert!(same_host("SRV1.corp.local", "srv1.corp.local", "web"));
        assert!(same_host("web", "10.0.0.1", "web"));
        assert!(!same_host("db1", "srv1.corp.local", "srv1"));
        assert!(!same_host("10.0.0.2", "10.0.0.1", "srv1"));
        assert!(!same_host("10.0.0.5", "10.0.0.1", "10"));
    }

    #[test]
    fn login_text() {
        let i18n = urfin_i18n::Catalog::embedded()
            .build(&"ru".parse().unwrap())
            .0;
        assert_eq!(
            TermLogin::Agent {
                algo: Some("ed25519".into())
            }
            .text(&i18n),
            "ключ ed25519 из агента"
        );
        assert_eq!(
            TermLogin::Agent { algo: None }.text(&i18n),
            "ключ из агента"
        );
    }
}
