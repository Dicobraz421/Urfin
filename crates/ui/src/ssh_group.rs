//! Экран группы ввода SSH без gpui (F03 §3.4, `screen-sshmulti`): строки колонки сеансов,
//! блоки вывода, чип, статус (№92) и клавиши (№88). Данные — `NetModel` (core) и
//! `TermState`; рисуют `gpui_shell::ssh_group` и чип в `gpui_shell::term_view`.

use std::sync::Arc;

use urfin_core::AppState;
use urfin_core::net::{Group, NetModel, TermLink, multi_exec_allowed};
use urfin_i18n::FluentArgs;
use urfin_term::vt::grid::{CursorKind, Snapshot};
use urfin_term::vt::shared::TermShared;

/// Меньше стольких строк блок вывода не бывает (F03 §3.4).
pub const MIN_LINES: usize = 6;

/// Группа и модель.
pub fn group(s: &AppState) -> Option<&Group> {
    s.ext.get::<NetModel>()?.group.as_ref()
}

/// Экран группы показан: панели спрятаны, не настройки.
pub fn shown(s: &AppState) -> bool {
    group(s).is_some_and(|g| g.on_screen)
        && s.ext
            .get::<crate::UiState>()
            .is_some_and(|u| u.panels_hidden && u.settings.is_none())
}

/// Сеанс `id` — в группе на экране: размер его PTY у экрана группы.
pub fn holds(s: &AppState, id: u32) -> bool {
    shown(s) && group(s).is_some_and(|g| g.members.contains(&id))
}

/// Имя сеанса: профиль, второй и дальше — с номером.
fn name(t: &TermLink) -> String {
    if t.n > 1 {
        format!("{} · {}", t.profile, t.n)
    } else {
        t.profile.clone()
    }
}

/// Адрес под именем: хост профиля.
fn host(m: &NetModel, t: &TermLink) -> String {
    m.profile(&t.profile)
        .map_or_else(String::new, |p| p.host.clone())
}

/// Строка колонки «Сеансы · группа ввода».
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: u32,
    pub name: String,
    pub host: String,
    pub on: bool,
    /// Курсор колонки (клавиши у неё).
    pub cursor: bool,
}

/// Все SSH-сеансы: флажок — в группе.
pub fn rows(s: &AppState) -> Vec<Row> {
    let Some(m) = s.ext.get::<NetModel>() else {
        return Vec::new();
    };
    let Some(g) = m.group.as_ref() else {
        return Vec::new();
    };
    let last = m.terms.len().saturating_sub(1);
    m.terms
        .iter()
        .enumerate()
        .map(|(i, t)| Row {
            id: t.id,
            name: name(t),
            host: host(m, t),
            on: g.members.contains(&t.id),
            cursor: g.list && i == g.cursor.min(last),
        })
        .collect()
}

/// Блок вывода сеанса группы.
#[derive(Clone, Debug)]
pub struct Block {
    pub id: u32,
    pub name: String,
    pub host: String,
    /// Код из OSC 133/633;D ([`code`]).
    pub exit: Option<i32>,
    pub shared: Arc<TermShared>,
}

/// Блоки сеансов группы: живые сеансы, а без них — сетки сцены.
pub fn blocks(s: &AppState) -> Vec<Block> {
    let Some(model) = s.ext.get::<NetModel>() else {
        return Vec::new();
    };
    let Some(group) = model.group.as_ref() else {
        return Vec::new();
    };
    let preview = |id: u32| {
        s.ext
            .get::<urfin_term::TermState>()?
            .preview_group
            .iter()
            .find(|p| p.id == id)
    };
    group
        .members
        .iter()
        .filter_map(|id| {
            let link = model.terms.iter().find(|t| t.id == *id)?;
            let (exit, shared) = if let Some(slot) = urfin_term::session_by_origin(s, *id) {
                (slot.last_exit.flatten(), slot.shared()?.clone())
            } else {
                let p = preview(*id)?;
                (p.exit, p.shared.clone())
            };
            Some(Block {
                id: *id,
                name: name(link),
                host: host(model, link),
                exit,
                shared,
            })
        })
        .collect()
}

/// «код N» заголовка блока и «не 0» (`danger`): только если сервер прислал код в OSC
/// 133/633;D, иначе без кода.
pub fn code(exit: Option<i32>) -> Option<(String, bool)> {
    let code = exit?;
    let mut a = FluentArgs::new();
    a.set("code", i64::from(code));
    Some((urfin_i18n::current().fmt("ui-group-code", &a), code != 0))
}

/// Чип заголовка окна: «Ввод в 3 сеанса:» и имена; только при включённом режиме группы.
pub fn chip(s: &AppState) -> Option<(String, String)> {
    let g = group(s).filter(|g| g.active())?;
    if !shown(s) {
        return None;
    }
    let names: Vec<String> = rows(s)
        .into_iter()
        .filter(|r| r.on)
        .map(|r| r.name)
        .collect();
    let mut a = FluentArgs::new();
    a.set("n", g.members.len());
    Some((
        urfin_i18n::current().fmt("ui-group-chip", &a),
        names.join(", "),
    ))
}

/// Строка подтверждения: «Отправить в 3 сеанса:», набранное, «? Enter — да, Esc — нет».
pub fn ask(s: &AppState) -> Option<(String, String, String)> {
    let g = group(s).filter(|g| g.asking())?;
    let i18n = urfin_i18n::current();
    let mut a = FluentArgs::new();
    a.set("n", g.members.len());
    Some((
        i18n.fmt("ui-group-ask", &a),
        g.typed.trim_end().to_owned(),
        i18n.get("ui-group-ask-tail"),
    ))
}

/// Статус экрана группы (`screen-sshmulti`): состав, подтверждение, размер.
pub fn status(s: &AppState) -> Option<[String; 3]> {
    if !shown(s) {
        return None;
    }
    let m = s.ext.get::<NetModel>()?;
    let g = m.group.as_ref()?;
    let i18n = urfin_i18n::current();
    let mut a = FluentArgs::new();
    a.set("n", g.members.len());
    a.set("total", m.terms.len());
    let left = i18n.fmt("ui-group-status", &a);
    let mid = g.confirmed.map_or_else(String::new, |at| {
        let mut a = FluentArgs::new();
        a.set("time", urfin_i18n::fmt::clock(at));
        i18n.fmt("ui-group-confirmed", &a)
    });
    let right = blocks(s).first().map_or_else(String::new, |b| {
        let size = b.shared.size();
        let mut a = FluentArgs::new();
        a.set("cols", i64::from(size.cols));
        a.set("rows", i64::from(size.rows));
        i18n.fmt("net-pair-size", &a)
    });
    Some([left, mid, right])
}

/// Клавиши экрана группы (№88): только то, что работает сейчас.
pub fn keys(s: &AppState) -> Option<Vec<(String, String)>> {
    if !shown(s) {
        return None;
    }
    let g = group(s)?;
    let i18n = urfin_i18n::current();
    let k = |key: &str, label: &str| (key.to_owned(), i18n.get(label));
    if g.asking() {
        return Some(vec![
            k("Enter", "ui-keys-group-send"),
            k("Esc", "ui-keys-group-cancel"),
        ]);
    }
    let mut keys = vec![
        k("Esc", "ui-keys-group-leave"),
        k("Ctrl+Alt+G", "ui-keys-group-edit"),
    ];
    if g.list {
        keys.push((i18n.get("ui-keys-space"), i18n.get("ui-keys-group-toggle")));
    }
    keys.push(k("Ctrl+Alt+H", "ui-keys-term-hl"));
    keys.extend(crate::term_rec::key(s, true));
    keys.push(k("Ctrl+O", "net-key-panels"));
    Some(keys)
}

/// Ctrl+Alt+G в keybar терминала: при `ssh.multi_exec=false` клавиши нет (№88).
pub fn key(s: &AppState) -> Option<(String, String)> {
    multi_exec_allowed(s).then(|| {
        (
            "Ctrl+Alt+G".to_owned(),
            urfin_i18n::current().get("ui-keys-group"),
        )
    })
}

/// Сколько строк выводить в блоке: высота делится поровну, минимум [`MIN_LINES`].
/// `avail` — высота стопки, `head` — высота заголовка с отступом, `line` — строка сетки.
pub fn fit_lines(avail: f32, n: usize, head: f32, line: f32) -> usize {
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "сеансов и строк в окне немного"
    )]
    let share = (avail / n.max(1) as f32 - head) / line.max(1.0);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "строк в окне немного"
    )]
    let fit = share.floor().max(0.0) as usize;
    fit.max(MIN_LINES)
}

/// Строк вывода в снапшоте: до последней непустой или до курсора.
pub fn content_rows(snap: &Snapshot) -> usize {
    let text = snap
        .cells
        .iter()
        .rposition(|r| r.iter().any(|c| c.ch != ' '))
        .map_or(0, |i| i + 1);
    text.max(usize::from(snap.cursor.0) + 1)
        .min(snap.cells.len())
}

/// Снапшот последних `fit` строк вывода его `Term`: короче — по содержимому, как в
/// `screen-sshmulti`; блоки прижаты к низу экрана.
pub fn tail(snap: &Snapshot, fit: usize) -> Snapshot {
    let n = content_rows(snap);
    let first = n.saturating_sub(fit);
    let cells: Vec<_> = snap.cells[first..n].to_vec();
    let row = usize::from(snap.cursor.0);
    let (cursor, cursor_kind) = if (first..n).contains(&row) {
        (
            (u16::try_from(row - first).unwrap_or(0), snap.cursor.1),
            snap.cursor_kind,
        )
    } else {
        ((0, 0), CursorKind::Hidden)
    };
    Snapshot {
        rows: u16::try_from(cells.len()).unwrap_or(u16::MAX),
        cells,
        cursor,
        cursor_kind,
        selection: Vec::new(),
        ..snap.clone()
    }
}

#[cfg(test)]
mod tests {
    use urfin_core::net::{Group, Profile, TermLogin};
    use urfin_core::{Location, Panel, Tab, TabId};
    use urfin_term::vt::grid::Cell;

    use super::*;

    fn link(id: u32, profile: &str, n: u32) -> TermLink {
        TermLink {
            id,
            profile: profile.into(),
            n,
            login: TermLogin::Password,
        }
    }

    fn state(members: &[u32], policy: Option<&str>) -> AppState {
        let here = Location::local(r"C:\w");
        let mut s = AppState::new(
            Panel::new(Tab::new(TabId(1), here.clone())),
            Panel::new(Tab::new(TabId(2), here)),
        );
        let mut m = NetModel {
            profiles: Some(vec![Profile::new("srv1", "10.20.4.11")]),
            ..NetModel::default()
        };
        m.terms.push(link(1, "srv1", 1));
        m.terms.push(link(2, "srv1", 2));
        m.terms.push(link(3, "srv3", 1));
        let mut g = Group::new(true, 1, None);
        for id in members {
            g.toggle(*id);
        }
        g.on_screen = true;
        m.group = Some(g);
        s.ext.insert(m);
        s.ext.insert(crate::UiState {
            panels_hidden: true,
            ..crate::UiState::default()
        });
        if let Some(p) = policy {
            s.ext.insert(urfin_core::PolicySet::from_toml(p).0);
        }
        s
    }

    #[test]
    fn rows_chip_and_status() {
        urfin_i18n::set_language("ru");
        let s = state(&[1, 2], None);
        let r = rows(&s);
        assert_eq!(r.len(), 3);
        assert_eq!(r[1].name, "srv1 · 2");
        assert_eq!(r[1].host, "10.20.4.11");
        assert!(r[0].on && r[1].on && !r[2].on);
        assert!(r[1].cursor && !r[0].cursor);
        let (lead, names) = chip(&s).unwrap();
        assert_eq!(lead, "Ввод в 2 сеанса:");
        assert_eq!(names, "srv1, srv1 · 2");
        assert_eq!(status(&s).unwrap()[0], "Группа: 2 сеанса из 3");
        assert!(chip(&state(&[1], None)).is_none(), "one session: no group");
    }

    /// Приёмка: «код N» — только если сервер прислал OSC 133/633;D с кодом.
    #[test]
    fn code_only_from_osc() {
        urfin_i18n::set_language("ru");
        assert_eq!(code(None), None, "no OSC D, or D without a code");
        assert_eq!(code(Some(0)), Some(("код 0".to_owned(), false)));
        assert_eq!(code(Some(1)), Some(("код 1".to_owned(), true)));
    }

    /// Приёмка: при `ssh.multi_exec=false` Ctrl+Alt+G нет в keybar.
    #[test]
    fn policy_hides_the_key() {
        urfin_i18n::set_language("ru");
        let on = state(&[], None);
        assert_eq!(key(&on).unwrap().0, "Ctrl+Alt+G");
        let off = state(&[], Some("[ssh]\nmulti_exec = false\n"));
        assert!(key(&off).is_none());
    }

    #[test]
    fn keys_follow_the_mode() {
        urfin_i18n::set_language("ru");
        let mut s = state(&[1, 2], None);
        let has = |s: &AppState, k: &str| keys(s).unwrap().iter().any(|(a, _)| a == k);
        assert!(has(&s, "Пробел"), "list has the keys");
        let edit = |s: &mut AppState, f: fn(&mut Group)| {
            if let Some(g) = s.ext.get_mut::<NetModel>().and_then(|m| m.group.as_mut()) {
                f(g);
            }
        };
        edit(&mut s, |g| g.list = false);
        assert!(!has(&s, "Пробел"), "space goes to the sessions");
        edit(&mut s, |g| g.held = Some(b"\r".to_vec()));
        assert_eq!(keys(&s).unwrap()[0].0, "Enter");
    }

    const BLANK: Cell = Cell {
        ch: ' ',
        fg: [0; 3],
        bg: [0; 3],
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        wide: false,
        spacer: false,
        plain: true,
    };

    fn snap(lines: &[&str], cursor: (u16, u16)) -> Snapshot {
        let row = |t: &str| {
            let mut r: Vec<Cell> = t.chars().map(|ch| Cell { ch, ..BLANK }).collect();
            r.resize(8, BLANK);
            r
        };
        Snapshot {
            cols: 8,
            rows: 6,
            cells: (0..6)
                .map(|i| row(lines.get(i).copied().unwrap_or("")))
                .collect(),
            cursor,
            cursor_kind: CursorKind::Block,
            cursor_color: [0; 3],
            bg: [0; 3],
            selection: vec![(0, 0, 1)],
            alt: false,
        }
    }

    #[test]
    fn tail_keeps_the_last_lines_and_cursor() {
        let s = snap(&["a", "b", "c", "$"], (3, 2));
        assert_eq!(content_rows(&s), 4);
        let t = tail(&s, 2);
        assert_eq!(t.rows, 2);
        assert_eq!(t.cells[0][0].ch, 'c');
        assert_eq!(t.cursor, (1, 2));
        assert_eq!(t.selection.len(), 0);
        let short = tail(&s, 10);
        assert_eq!(short.rows, 4, "shorter than the share: by content");
        let up = tail(&snap(&["a", "b", "c", "d"], (0, 0)), 2);
        assert_eq!(up.cursor_kind, CursorKind::Hidden, "cursor above the tail");
    }

    #[test]
    fn share_is_even_with_a_minimum() {
        assert_eq!(fit_lines(600.0, 3, 38.0, 17.0), 9);
        assert_eq!(fit_lines(300.0, 6, 38.0, 17.0), MIN_LINES);
    }
}
