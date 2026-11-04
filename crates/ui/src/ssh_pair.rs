//! Пара «панель + терминал» SSH без gpui (F03 §3.2, `screen-sshpair`): статус пары одной
//! группой (№92) и клавиши экрана (№88), сеанс и размер терминала. Данные — `NetModel`
//! (core) и `TermState`; рисует `gpui_shell::ssh_pair`.

use std::sync::Arc;

use urfin_core::AppState;
use urfin_core::net::{NetModel, Pair, PairCwd, TermLink, TermLogin, pair_term_focused};
use urfin_i18n::FluentArgs;
use urfin_term::vt::shared::TermShared;

/// Пара и её модель.
pub fn pair(s: &AppState) -> Option<&Pair> {
    s.ext.get::<NetModel>()?.pair.as_ref()
}

/// Сеанс терминала пары.
pub fn link(s: &AppState) -> Option<&TermLink> {
    let id = pair(s)?.term?;
    s.ext.get::<NetModel>()?.terms.iter().find(|t| t.id == id)
}

/// Сетка терминала пары: её сеанс, а без него — показанная явно (сцены `--screens`).
pub fn shared(s: &AppState) -> Option<&Arc<TermShared>> {
    let p = pair(s)?;
    let live = p
        .term
        .and_then(|id| urfin_term::session_by_origin(s, id))
        .and_then(urfin_term::manager::Slot::shared);
    live.or_else(|| s.ext.get::<urfin_term::TermState>()?.preview.as_ref())
}

/// Сеансы профиля пары — меню шапки «ssh srv1 ▾».
pub fn sessions(s: &AppState) -> Vec<&TermLink> {
    let Some(p) = pair(s) else {
        return Vec::new();
    };
    s.ext
        .get::<NetModel>()
        .map(|m| {
            m.terms
                .iter()
                .filter(|t| t.profile.eq_ignore_ascii_case(&p.profile))
                .collect()
        })
        .unwrap_or_default()
}

/// «ssh srv1» — шапка терминала.
pub fn title(p: &Pair) -> String {
    format!("ssh {}", p.profile)
}

/// Статус пары, когда фокус у её терминала: «ssh.exe · ключ ed25519 из агента» |
/// «папка: /var/www/app ↔ панель» | «UTF-8 · 79×18».
pub fn status(s: &AppState) -> Option<[String; 3]> {
    if !pair_term_focused(s) {
        return None;
    }
    let p = pair(s)?;
    let i18n = urfin_i18n::current();
    let login = link(s).map_or(TermLogin::Agent { algo: None }, |l| l.login.clone());
    let mut a = FluentArgs::new();
    a.set("login", login.text(&i18n));
    let left = i18n.fmt("net-pair-exe", &a);
    let mid = match &p.cwd {
        PairCwd::At(path) => {
            let mut a = FluentArgs::new();
            a.set("path", path.clone());
            i18n.fmt("net-pair-cwd", &a)
        }
        PairCwd::Silent => i18n.get("net-pair-cwd-silent"),
        PairCwd::Wait => i18n.get("net-pair-cwd-wait"),
    };
    let right = shared(s).map_or_else(String::new, |sh| {
        let size = sh.size();
        let mut a = FluentArgs::new();
        a.set("cols", i64::from(size.cols));
        a.set("rows", i64::from(size.rows));
        i18n.fmt("net-pair-size", &a)
    });
    Some([left, mid, right])
}

/// Клавиши экрана пары в keybar (№88): только то, что работает.
pub fn keys(s: &AppState) -> Option<Vec<(String, String)>> {
    if !pair_term_focused(s) {
        return None;
    }
    let i18n = urfin_i18n::current();
    let k = |key: &str, label: &str| (key.to_owned(), i18n.get(label));
    Some(vec![
        k("Ctrl+O", "net-key-panels"),
        k("Ctrl+Tab", "net-key-pair-focus"),
        k("Ctrl+Alt+H", "ui-keys-term-hl"),
    ])
}

/// Сколько высоты панели `side` занимает терминал пары (шапка и сетка): курсор панели
/// держится над ним.
pub fn extra_h(s: &AppState, side: urfin_core::Side, h: f32) -> f32 {
    match pair(s) {
        Some(p) if p.side == side => h * (1.0 - p.split.clamp(0.2, 0.8)),
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use urfin_core::net::{Pair, PairCwd, TermLink, TermLogin};
    use urfin_core::{AppState, Focus, Side};

    use super::*;

    fn state(focus_term: bool) -> AppState {
        use urfin_core::{Location, Panel, Tab, TabId};
        let here = Location::local(r"C:\w");
        let mut s = AppState::new(
            Panel::new(Tab::new(TabId(1), here.clone())),
            Panel::new(Tab::new(TabId(2), here)),
        );
        let mut m = NetModel::default();
        m.terms.push(TermLink {
            id: 3,
            profile: "srv1".into(),
            n: 1,
            login: TermLogin::Agent {
                algo: Some("ed25519".into()),
            },
        });
        m.pair = Some(Pair {
            term: Some(3),
            focus_term,
            cwd: PairCwd::At("/var/www/app".into()),
            ..Pair::new("srv1", Side::Right)
        });
        s.ext.insert(m);
        s.focus = if focus_term {
            Focus::Terminal
        } else {
            Focus::Panel
        };
        s
    }

    #[test]
    fn status_and_keys_when_terminal_focused() {
        urfin_i18n::set_language("ru");
        let s = state(true);
        let st = status(&s).unwrap();
        assert_eq!(st[0], "ssh.exe · ключ ed25519 из агента");
        assert_eq!(st[1], "папка: /var/www/app ↔ панель");
        let k = keys(&s).unwrap();
        assert_eq!(k[1], ("Ctrl+Tab".into(), "Панель ↔ терминал".into()));
        assert!(
            status(&state(false)).is_none(),
            "panel focus: normal status"
        );
        assert_eq!(extra_h(&s, Side::Right, 100.0).round(), 55.0);
        assert!(extra_h(&s, Side::Left, 100.0).abs() < f32::EPSILON);
        assert_eq!(sessions(&s).len(), 1);
    }
}
