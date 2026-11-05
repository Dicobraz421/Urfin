//! Группа ввода мультивыполнения SSH (F03 §3.4, `screen-sshmulti`): состав, колонка
//! сеансов, задержка первого Enter. Только данные и правила; байты в PTY шлёт
//! `urfin-term`, команды `ssh.group.*` приходят из `urfin-net`, рисует `urfin-ui`.

use crate::{AppState, Focus, PolicyKey, PolicySet};

/// Группа из стольких сеансов включает режим группы.
pub const GROUP_MIN: usize = 2;

/// Начало скобок вставки: такая вставка держится вся, а не до первого перевода строки.
const PASTE_START: &[u8] = b"\x1b[200~";

/// `Ctrl+U`: «нет» в строке подтверждения стирает набранное во всех сеансах группы.
const KILL_LINE: u8 = 0x15;

/// Группа ввода: `id` — сеансы `urfin-term` ([`super::TermLink::id`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Group {
    /// Сеансы группы по возрастанию: в этом порядке идёт ввод.
    pub members: Vec<u32>,
    /// Строка колонки «Сеансы · группа ввода» (индекс в `NetModel::terms`).
    pub cursor: usize,
    /// Клавиши — у колонки (Ctrl+Alt+G): ↑↓, Пробел, Enter; иначе ввод в PTY группы.
    pub list: bool,
    /// Экран группы показан (панели спрятаны): только тогда ввод идёт во все сеансы.
    pub on_screen: bool,
    /// `[ssh].multi_exec_confirm`: первый Enter спрашивает.
    pub ask: bool,
    /// Первая команда подтверждена (секунды Unix); сбрасывается сменой состава.
    pub confirmed: Option<i64>,
    /// Набранное с последнего Enter — для строки подтверждения.
    pub typed: String,
    /// Задержанный Enter (и хвост вставки): ждёт ответа.
    pub held: Option<Vec<u8>>,
    /// Активный сеанс терминала до группы (локальный): вернётся при выходе.
    pub back: Option<u32>,
}

impl Group {
    pub fn new(ask: bool, cursor: usize, back: Option<u32>) -> Group {
        Group {
            cursor,
            list: true,
            ask,
            back,
            ..Group::default()
        }
    }

    /// Режим группы включён: ввод идёт во все её сеансы.
    pub fn active(&self) -> bool {
        self.members.len() >= GROUP_MIN
    }

    /// Открыта строка «Отправить в N сеансов?».
    pub fn asking(&self) -> bool {
        self.held.is_some()
    }

    /// Состав изменился: снова спросить, набранное не считается.
    fn changed(&mut self) {
        self.confirmed = None;
        self.typed.clear();
        self.held = None;
        if !self.active() {
            self.list = true;
        }
    }

    /// Пробел в колонке: сеанс в группу или из группы.
    pub fn toggle(&mut self, id: u32) {
        match self.members.binary_search(&id) {
            Ok(i) => {
                self.members.remove(i);
            }
            Err(i) => self.members.insert(i, id),
        }
        self.changed();
    }

    /// Сеанс закрыт.
    pub fn remove(&mut self, id: u32) {
        if let Ok(i) = self.members.binary_search(&id) {
            self.members.remove(i);
            self.changed();
        }
    }

    fn needs_ok(&self) -> bool {
        self.ask && self.confirmed.is_none()
    }

    /// Байты ввода группы → что уходит во все PTY сейчас. Первый перевод строки
    /// задерживается ([`Group::asking`]); пока идёт вопрос, Enter — «да» (момент `now`),
    /// остальной ввод не идёт никуда.
    pub fn feed(&mut self, bytes: &[u8], now: i64) -> Vec<u8> {
        if self.asking() {
            return if bytes == b"\r" {
                self.confirmed = Some(now);
                self.typed.clear();
                self.held.take().unwrap_or_default()
            } else {
                Vec::new()
            };
        }
        let eol = bytes.iter().position(|b| matches!(b, b'\r' | b'\n'));
        self.track(bytes);
        let Some(eol) = eol.filter(|_| self.needs_ok()) else {
            if eol.is_some() {
                self.typed.clear();
            }
            return bytes.to_vec();
        };
        // Вставка в скобках — целиком: открытые скобки без закрывающих портят ввод shell.
        let at = if bytes.starts_with(PASTE_START) {
            0
        } else {
            eol
        };
        self.held = Some(bytes[at..].to_vec());
        bytes[..at].to_vec()
    }

    /// Esc в строке вопроса: Enter не ушёл, набранное стирается во всех сеансах.
    pub fn reject(&mut self) -> Vec<u8> {
        self.held = None;
        self.typed.clear();
        vec![KILL_LINE]
    }

    /// Набранное по байтам ввода: правка конца строки — как в оболочке; стрелки и прочие
    /// последовательности в подпись не попадают.
    fn track(&mut self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        let mut it = text.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '\u{1b}' => {
                    // CSI и SS3: ESC, вводный символ, параметры, конечный символ.
                    if it.next_if(|c| matches!(c, '[' | 'O')).is_some() {
                        while it
                            .next_if(|c| !c.is_ascii_alphabetic() && *c != '~')
                            .is_some()
                        {}
                        it.next();
                    }
                }
                '\u{7f}' | '\u{8}' => {
                    self.typed.pop();
                }
                '\u{15}' | '\u{3}' => self.typed.clear(),
                '\u{17}' => {
                    let t = self.typed.trim_end();
                    let keep = t.rfind(' ').map_or(0, |i| i + 1);
                    self.typed.truncate(keep);
                }
                '\r' | '\n' | '\t' => self.typed.push(' '),
                c if c.is_control() => {}
                c => self.typed.push(c),
            }
        }
    }
}

/// Контекст keymap `ssh_group` (F03 §3.4): экран группы и фокус у терминала.
pub fn in_ssh_group(st: &AppState) -> bool {
    st.focus == Focus::Terminal && group_on_screen(st)
}

/// Экран группы показан.
pub fn group_on_screen(st: &AppState) -> bool {
    st.ext
        .get::<super::NetModel>()
        .and_then(|m| m.group.as_ref())
        .is_some_and(|g| g.on_screen)
}

/// Политика `ssh.multi_exec` (№88): нет значения — можно.
pub fn multi_exec_allowed(st: &AppState) -> bool {
    st.ext
        .get::<PolicySet>()
        .and_then(|p| p.bool(PolicyKey::SshMultiExec))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group() -> Group {
        let mut g = Group::new(true, 0, None);
        g.toggle(3);
        g.toggle(1);
        g.list = false;
        g
    }

    #[test]
    fn two_sessions_turn_the_group_on_in_order() {
        let mut g = Group::new(true, 0, None);
        g.toggle(3);
        assert!(!g.active() && g.list);
        g.toggle(1);
        assert!(g.active());
        assert_eq!(g.members, [1, 3]);
        g.list = false;
        g.toggle(3);
        assert!(g.list, "one session left: back to the list");
    }

    #[test]
    fn first_enter_waits_for_yes() {
        let mut g = group();
        assert_eq!(g.feed(b"up", 10), b"up");
        assert_eq!(g.feed(b"time", 10), b"time");
        assert_eq!(g.feed(b"\r", 10), b"");
        assert!(g.asking());
        assert_eq!(g.typed, "uptime ");
        assert_eq!(
            g.feed(b"x", 11),
            b"",
            "nothing goes out during the question"
        );
        assert_eq!(g.feed(b"\r", 12), b"\r");
        assert!(!g.asking());
        assert_eq!(g.confirmed, Some(12));
        assert_eq!(g.feed(b"ls", 13), b"ls");
        assert_eq!(g.feed(b"\r", 13), b"\r", "after yes Enter goes through");
    }

    #[test]
    fn no_sends_kill_line() {
        let mut g = group();
        g.feed(b"reboot", 1);
        assert_eq!(g.feed(b"\r", 1), b"");
        assert_eq!(g.reject(), [0x15]);
        assert!(!g.asking() && g.typed.is_empty() && g.confirmed.is_none());
        assert_eq!(g.feed(b"\r", 2), b"", "still unconfirmed");
    }

    #[test]
    fn changing_the_group_asks_again() {
        let mut g = group();
        g.feed(b"\r", 1);
        g.feed(b"\r", 1);
        assert_eq!(g.feed(b"\r", 2), b"\r");
        g.toggle(5);
        assert_eq!(g.confirmed, None);
        assert_eq!(g.feed(b"\r", 3), b"");
    }

    #[test]
    fn setting_off_never_asks() {
        let mut g = group();
        g.ask = false;
        assert_eq!(g.feed(b"ls\r", 1), b"ls\r");
        assert!(!g.asking() && g.typed.is_empty());
    }

    #[test]
    fn paste_holds_from_the_first_line_end() {
        let mut g = group();
        assert_eq!(g.feed(b"a\rb\r", 1), b"a");
        assert_eq!(g.held.as_deref(), Some(&b"\rb\r"[..]));
        assert_eq!(g.feed(b"\r", 2), b"\rb\r");
        let mut g = group();
        let paste = b"\x1b[200~a\rb\x1b[201~";
        assert_eq!(g.feed(paste, 1), b"", "bracketed paste stays whole");
        assert_eq!(g.typed, "a b");
        assert_eq!(g.feed(b"\r", 2), paste);
    }

    #[test]
    fn typed_follows_line_editing() {
        let mut g = group();
        for b in [&b"git pull"[..], b"\x7f", b"l", b"\x1b[A", b"\x1bOB"] {
            g.feed(b, 1);
        }
        assert_eq!(g.typed, "git pull");
        g.feed(b"\x17", 1);
        assert_eq!(g.typed, "git ");
        g.feed("привет".as_bytes(), 1);
        assert_eq!(g.typed, "git привет");
        g.feed(b"\x15", 1);
        assert_eq!(g.typed, "");
    }
}
