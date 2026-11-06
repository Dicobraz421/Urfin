//! Пара «панель + терминал» SSH в окне (F03 §3.2, `screen-sshpair`): во второй половине
//! окна сверху панель `sftp://`, под ней терминал профиля с шапкой «ssh srv1 ▾» (меню
//! сеансов профиля, «Новый сеанс», «Перейти в терминале сюда»). Сетка — та же, что у
//! терминала за панелями (`term_view::view`); размер PTY пары — `term.pin`. Статус и
//! клавиши — `crate::ssh_pair` (без gpui).

use std::rc::Rc;

use gpui_kit::prelude::*;
use gpui_kit::{Context, Pixels, SharedString, Window, div, px, relative};
use urfin_core::{AppState, Command};
use urfin_term::vt::grid::Snapshot;

use super::CoreHost;
use super::features::{Frame, Main, ShellFeature};
use super::screens::Scene;
use super::term_view::{self, GridBounds, PAD_X, PAD_Y};
use super::theme::Theme;
use crate::tokens::{Metric, Role};

/// Высота шапки терминала пары.
const H_HEAD: f32 = 32.0;
/// Ширина меню шапки.
const W_MENU: f32 = 260.0;

/// Фича окна: терминал пары под её панелью.
#[derive(Default)]
pub(super) struct SshPair {
    /// Снапшот сетки с подсветкой вывода (F03-10).
    grid: term_view::Grid,
    bounds: GridBounds,
    /// Метрики ячейки (по окну, в проходе Core).
    cell: Option<(Pixels, Pixels)>,
    /// Размер PTY, отправленный `term.pin`: сеанс, колонки, строки.
    pinned: Option<(u32, u16, u16)>,
    /// Пара видна в последнем кадре.
    shown: bool,
    menu: bool,
}

/// Пара показана: панели видны, не настройки и не вьювер на всё окно.
fn visible(s: &AppState, hidden: bool) -> bool {
    !hidden
        && crate::ssh_pair::pair(s).is_some()
        && s.ext
            .get::<crate::UiState>()
            .is_none_or(|u| u.settings.is_none())
        && super::view::shown(s).is_none()
}

impl SshPair {
    fn snapshot(&mut self, theme: &Theme, s: &AppState) -> Option<Rc<Snapshot>> {
        let shared = crate::ssh_pair::shared(s)?;
        Some(self.grid.get(shared, theme, s))
    }

    /// Ячеек в области сетки (по её последним границам).
    fn want(&self) -> Option<(u16, u16)> {
        let b = self.bounds.get()?;
        let (cw, ch) = self.cell?;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "ячеек в окне — до тысяч"
        )]
        let cells = |len: f32, pad: f32, cell: Pixels| {
            ((len - 2.0 * pad) / f32::from(cell)).floor().max(2.0) as u16
        };
        Some((
            cells(f32::from(b.size.width), PAD_X, cw),
            cells(f32::from(b.size.height), PAD_Y, ch),
        ))
    }
}

/// Клик фичи — через хост.
fn with_me(h: &mut CoreHost, f: impl FnOnce(&mut SshPair)) {
    if let Some(me) = h.features.get_mut::<SshPair>() {
        f(me);
    }
}

fn menu_item(
    theme: &Theme,
    id: String,
    label: String,
    current: bool,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(SharedString::from(id))
        .h(px(theme.m(Metric::HMenuItem)))
        .px(px(12.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .rounded(px(theme.m(Metric::RadiusControl)))
        .hover(|d| d.bg(theme.c(Role::FillSubtleHover)))
        .text_color(theme.c(Role::TextPrimary))
        .child(
            div()
                .w(px(12.0))
                .text_color(theme.c(Role::AccentText))
                .child(if current { "✓" } else { "" }),
        )
        .child(label)
}

impl ShellFeature for SshPair {
    fn stale(&self) -> bool {
        self.grid.stale()
    }

    /// Метрики ячейки — по окну (размер PTY пары считает `due`).
    fn sync(&mut self, theme: &Theme, window: &mut Window) -> Option<Command> {
        self.cell = Some(term_view::metrics(theme.m(Metric::TypeMono), window));
        None
    }

    /// Размер PTY пары по её области; пара спрятана — снова размер окна.
    fn due(&mut self, s: &AppState, _now: std::time::Instant) -> Vec<Command> {
        let term = crate::ssh_pair::pair(s).and_then(|p| p.term);
        let live = term.filter(|id| urfin_term::session_by_origin(s, *id).is_some());
        match (live, self.shown) {
            (Some(id), true) => {
                let Some((cols, rows)) = self.want() else {
                    return Vec::new();
                };
                if self.pinned == Some((id, cols, rows)) {
                    return Vec::new();
                }
                self.pinned = Some((id, cols, rows));
                vec![
                    Command::new("term.pin")
                        .arg("id", i64::from(id))
                        .arg("cols", i64::from(cols))
                        .arg("rows", i64::from(rows)),
                ]
            }
            _ => match self.pinned.take() {
                // Сеанс ушёл на экран группы: размер теперь у него.
                Some((id, ..))
                    if urfin_term::session_by_origin(s, id).is_some()
                        && !crate::ssh_group::holds(s, id) =>
                {
                    vec![Command::new("term.pin").arg("id", i64::from(id))]
                }
                _ => Vec::new(),
            },
        }
    }

    fn main(
        &mut self,
        f: &Frame<'_>,
        hidden: bool,
        window: &mut Window,
        cx: &Context<CoreHost>,
    ) -> Option<Main> {
        let (s, theme) = (f.s, f.theme);
        self.shown = visible(s, hidden);
        if !self.shown {
            self.grid.clear();
            self.menu = false;
            return None;
        }
        let pair = crate::ssh_pair::pair(s)?.clone();
        let widths = CoreHost::layout(theme, s, f32::from(window.viewport_size().width));
        let right = usize::from(pair.side == urfin_core::Side::Right);
        let (left, width) = CoreHost::panel_spans(theme, widths)[right]?;
        let snap = self.snapshot(theme, s);
        let profile = pair.profile.clone();
        let grid = div()
            .id("sshpair-grid")
            .flex_1()
            .min_h_0()
            .relative()
            .children(snap.map(|snap| {
                div().absolute().inset_0().child(term_view::view(
                    snap,
                    theme.m(Metric::TypeMono),
                    self.bounds.clone(),
                ))
            }))
            // Клик в терминал — фокус ему (ввод сырой, Ctrl+Tab — обратно).
            .on_mouse_down(gpui_kit::MouseButton::Left, {
                let profile = profile.clone();
                cx.listener(move |h, _: &gpui_kit::MouseDownEvent, w, cx| {
                    if !h
                        .core
                        .as_ref()
                        .is_some_and(|c| urfin_core::net::pair_term_focused(c.state()))
                    {
                        h.run(
                            Command::new("ssh.terminal").arg("profile", profile.clone()),
                            w,
                            cx,
                        );
                    }
                })
            });
        let pane = div()
            .absolute()
            .left(px(left))
            .w(px(width))
            .top(relative(pair.split.clamp(0.2, 0.8)))
            .bottom_0()
            .flex()
            .flex_col()
            .bg(theme.c(Role::BgBase))
            .child(head(
                theme,
                &pair,
                term_view::hl_on(s),
                crate::ssh_pair::rec(s),
                cx,
            ))
            .child(grid)
            .children(self.menu.then(|| menu(s, theme, &pair, cx)));
        Some(Main::Over(pane.into_any_element()))
    }
}

/// Шапка терминала пары: «ssh srv1 ▾» — меню сеансов профиля.
fn head(
    theme: &Theme,
    pair: &urfin_core::net::Pair,
    hl: bool,
    rec: Option<&urfin_term::rec::Recording>,
    cx: &Context<CoreHost>,
) -> gpui_kit::Div {
    div()
        .h(px(H_HEAD))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(8.0))
        .px(px(8.0))
        .bg(theme.c(Role::LayerPanel))
        .border_t_1()
        .border_b_1()
        .border_color(theme.c(Role::StrokeDivider))
        .child(
            div()
                .id("sshpair-head")
                .h(px(24.0))
                .px(px(8.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .rounded(px(theme.m(Metric::RadiusControl)))
                .border_1()
                .border_color(theme.c(Role::StrokeControl))
                .bg(theme.c(Role::FillControl))
                .hover(|d| d.bg(theme.c(Role::FillSubtleHover)))
                .font_family("Cascadia Mono")
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_size(px(theme.m(Metric::TypeBody)))
                .child(crate::ssh_pair::title(pair))
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(theme.c(Role::TextSecondary))
                        .child("▾"),
                )
                .on_click(cx.listener(|h, _: &gpui_kit::ClickEvent, _w, cx| {
                    with_me(h, |me| me.menu = !me.menu);
                    cx.notify();
                })),
        )
        // Чипы записи и подсветки вывода (№91) — справа в шапке терминала пары.
        .child(div().flex_1())
        .children(rec.map(|r| term_view::rec_chip(theme, r)))
        .children(hl.then(|| term_view::hl_chip(theme)))
}

/// Меню шапки: сеансы профиля, «Новый сеанс», «Перейти в терминале сюда».
fn menu(
    s: &AppState,
    theme: &Theme,
    pair: &urfin_core::net::Pair,
    cx: &Context<CoreHost>,
) -> gpui_kit::Div {
    let i18n = urfin_i18n::current();
    let profile = pair.profile.clone();
    let item = |id: String, label: String, current: bool, cmd: Command| {
        menu_item(theme, id, label, current).on_click(cx.listener(
            move |h, _: &gpui_kit::ClickEvent, w, cx| {
                with_me(h, |me| me.menu = false);
                h.run(cmd.clone(), w, cx);
            },
        ))
    };
    let mut list = div()
        .absolute()
        .top(px(H_HEAD - 2.0))
        .left(px(8.0))
        .w(px(W_MENU))
        .p(px(4.0))
        .flex()
        .flex_col()
        .bg(theme.c(Role::LayerOverlay))
        .border_1()
        .border_color(theme.c(Role::StrokeCard))
        .rounded(px(theme.m(Metric::RadiusOverlay)))
        .shadow_md()
        .occlude();
    for t in crate::ssh_pair::sessions(s) {
        let mut a = urfin_i18n::FluentArgs::new();
        a.set("profile", t.profile.clone());
        a.set("n", i64::from(t.n));
        let cmd = Command::new("ssh.terminal")
            .arg("profile", profile.clone())
            .arg("session", i64::from(t.id));
        list = list.child(item(
            format!("sshpair-s{}", t.id),
            i18n.fmt("net-pair-session", &a),
            pair.term == Some(t.id),
            cmd,
        ));
    }
    let new = Command::new("ssh.terminal")
        .arg("profile", profile)
        .arg("new", true);
    list.child(item(
        "sshpair-new".into(),
        i18n.get("net-pair-new"),
        false,
        new,
    ))
    .child(
        div()
            .h(px(1.0))
            .my(px(4.0))
            .bg(theme.c(Role::StrokeDivider)),
    )
    .child(item(
        "sshpair-cd".into(),
        i18n.get("cmd-ssh-cd_here"),
        false,
        Command::new("ssh.cd_here"),
    ))
}

/// Сцена пары (`docs/design/screens/screen-sshpair.png`): слева локальная папка, справа
/// `sftp://srv1/var/www/app`, под ней терминал с выводом `git pull`, `systemctl` и
/// журнала; фокус у терминала — статус пары и её клавиши.
pub(super) const SCENES: &[Scene] = &[
    Scene {
        name: "sshpair-open",
        steps: &[
            "ui.cancel",
            "resize 1280x760",
            "screens.ssh kind=sftp",
            "panel.go side=right path=sftp://srv1/var/www/app",
            "panel.focus side=right",
        ],
    },
    Scene {
        name: "screen-sshpair",
        steps: &["screens.ssh kind=pair"],
    },
    Scene {
        name: "sshpair-closed",
        steps: &[
            "screens.ssh kind=clear",
            "panel.back",
            "panel.focus side=left",
            "resize 1200x760",
        ],
    },
];
