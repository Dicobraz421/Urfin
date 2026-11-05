//! Экран группы ввода SSH (F03 §3.4, `screen-sshmulti`): слева колонка «Сеансы · группа
//! ввода» шириной 200 с флажками, справа стопка блоков по сеансам группы — заголовок
//! (имя, адрес, «код N») и последние строки его `Term`; над вводом — строка подтверждения
//! первой команды. Строки и правила — `crate::ssh_group` (без gpui); ввод — в `urfin-term`.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use gpui_kit::prelude::*;
use gpui_kit::{Context, Pixels, SharedString, Window, div, px};
use urfin_core::{AppState, Command};

use super::CoreHost;
use super::features::{Frame, Main, ShellFeature};
use super::screens::Scene;
use super::term_view::{self, PAD_Y};
use super::theme::Theme;
use crate::ssh_group::{Block, Row};
use crate::tokens::{Metric, Role};

/// Ширина колонки сеансов.
const W_LIST: f32 = 200.0;
/// Поля стопки блоков: сверху и снизу, слева и справа.
const PAD_OUT_Y: f32 = 8.0;
const PAD_OUT_X: f32 = 12.0;
/// Заголовок блока и отступ над ним.
const H_HEAD: f32 = 24.0;
const GAP_HEAD: f32 = 10.0;
/// Строка подтверждения.
const H_ASK: f32 = 46.0;

/// Фича окна: экран группы.
#[derive(Default)]
pub(super) struct SshGroup {
    /// Снапшоты сеансов с подсветкой вывода (F03-10).
    grids: HashMap<u32, term_view::Grid>,
    /// Метрики ячейки и размер окна (в проходе Core).
    cell: Option<(Pixels, Pixels)>,
    width: f32,
    /// Размеры PTY, отправленные `term.pin`: сеанс → колонки, строки.
    pinned: BTreeMap<u32, (u16, u16)>,
    shown: bool,
}

impl SshGroup {
    /// Колонок в блоке вывода.
    fn cols(&self) -> Option<u16> {
        let (cw, _) = self.cell?;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "ячеек в окне — до тысяч"
        )]
        let cols = ((self.width - W_LIST - 2.0 * PAD_OUT_X) / f32::from(cw))
            .floor()
            .max(2.0) as u16;
        Some(cols)
    }
}

impl ShellFeature for SshGroup {
    /// Метрики ячейки и ширина окна (размер PTY считает `due`).
    fn sync(&mut self, theme: &Theme, window: &mut Window) -> Option<Command> {
        self.cell = Some(term_view::metrics(theme.m(Metric::TypeMono), window));
        self.width = f32::from(window.viewport_size().width);
        None
    }

    /// Сеансы группы — шириной блока вывода, высотой области терминала (`screen-sshmulti`:
    /// 136×31); вне группы — снова размер окна.
    fn due(&mut self, s: &AppState, _now: std::time::Instant) -> Vec<Command> {
        let mut out = Vec::new();
        let area = s
            .ext
            .get::<urfin_term::TermState>()
            .and_then(|t| t.sessions.area());
        let want: BTreeMap<u32, (u16, u16)> = match (self.shown, self.cols(), area) {
            (true, Some(cols), Some(area)) => crate::ssh_group::group(s)
                .map(|g| {
                    g.members
                        .iter()
                        .filter(|id| urfin_term::session_by_origin(s, **id).is_some())
                        .map(|id| (*id, (cols, area.rows)))
                        .collect()
                })
                .unwrap_or_default(),
            _ => BTreeMap::new(),
        };
        self.pinned.retain(|id, _| {
            let keep = want.contains_key(id);
            if !keep && urfin_term::session_by_origin(s, *id).is_some() {
                out.push(Command::new("term.pin").arg("id", i64::from(*id)));
            }
            keep
        });
        for (id, (cols, rows)) in want {
            if self.pinned.insert(id, (cols, rows)) != Some((cols, rows)) {
                out.push(
                    Command::new("term.pin")
                        .arg("id", i64::from(id))
                        .arg("cols", i64::from(cols))
                        .arg("rows", i64::from(rows)),
                );
            }
        }
        out
    }

    fn main(
        &mut self,
        f: &Frame<'_>,
        hidden: bool,
        window: &mut Window,
        cx: &Context<CoreHost>,
    ) -> Option<Main> {
        let (s, theme) = (f.s, f.theme);
        self.shown = hidden && crate::ssh_group::shown(s);
        if !self.shown {
            self.grids.clear();
            return None;
        }
        let (_, ch) = term_view::metrics(theme.m(Metric::TypeMono), window);
        let ask = crate::ssh_group::ask(s);
        let avail = f32::from(window.viewport_size().height)
            - theme.m(Metric::HTitlebar)
            - theme.m(Metric::HStatus)
            - theme.m(Metric::HKeybar)
            - 2.0 * PAD_OUT_Y
            - if ask.is_some() { H_ASK } else { 0.0 };
        let bg = term_view::hsla(term_view::defaults(theme.tokens.dark()).bg);
        Some(Main::Cover(
            div()
                .id("ssh-group")
                .absolute()
                .inset_0()
                .flex()
                .flex_row()
                .bg(bg)
                .child(column(s, theme, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .flex()
                        .flex_col()
                        .child(self.stack(s, theme, avail, ch))
                        .children(ask.map(|a| ask_line(theme, a))),
                )
                .into_any_element(),
        ))
    }
}

impl SshGroup {
    /// Стопка блоков: высота поровну, минимум 6 строк; что короче доли — по содержимому,
    /// прижато к низу. Не вошло — прокрутка.
    fn stack(
        &mut self,
        s: &AppState,
        theme: &Theme,
        avail: f32,
        ch: Pixels,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let blocks = crate::ssh_group::blocks(s);
        let head = H_HEAD + GAP_HEAD + PAD_Y;
        let fit = crate::ssh_group::fit_lines(avail, blocks.len(), head, f32::from(ch));
        let mut used = 0.0;
        let mut els = Vec::with_capacity(blocks.len());
        self.grids
            .retain(|id, _| blocks.iter().any(|b| b.id == *id));
        for b in &blocks {
            let grid = self.grids.entry(b.id).or_default();
            let snap = grid.get(&b.shared, theme, s);
            let tail = crate::ssh_group::tail(&snap, fit);
            #[allow(clippy::cast_precision_loss, reason = "строк в окне немного")]
            let h = f32::from(ch) * tail.cells.len() as f32 + PAD_Y;
            used += h + H_HEAD + GAP_HEAD;
            els.push(block(theme, b, tail, h));
        }
        div()
            .id("ssh-group-out")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .when(used <= avail, gpui_kit::Styled::justify_end)
            .overflow_y_scroll()
            .px(px(PAD_OUT_X - term_view::PAD_X))
            .py(px(PAD_OUT_Y))
            .children(els)
    }
}

/// Колонка «Сеансы · группа ввода»: клик по строке — в группу или из группы.
fn column(s: &AppState, theme: &Theme, cx: &Context<CoreHost>) -> gpui_kit::Div {
    let i18n = urfin_i18n::current();
    div()
        .w(px(W_LIST))
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .px(px(6.0))
        .py(px(8.0))
        .bg(theme.c(Role::Header))
        .border_r_1()
        .border_color(theme.c(Role::StrokeDivider))
        .text_size(px(13.0))
        .child(
            div()
                .px(px(8.0))
                .pt(px(2.0))
                .pb(px(6.0))
                .text_size(px(11.5))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(i18n.get("ui-group-title"))),
        )
        .children(
            crate::ssh_group::rows(s)
                .into_iter()
                .map(|r| row(theme, r, cx)),
        )
}

fn row(theme: &Theme, r: Row, cx: &Context<CoreHost>) -> gpui_kit::Stateful<gpui_kit::Div> {
    let id = r.id;
    div()
        .id(SharedString::from(format!("ssh-group-{id}")))
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(8.0))
        .py(px(6.0))
        .rounded(px(4.0))
        .when(r.cursor, |d| d.bg(theme.c(Role::FillSubtleHover)))
        .text_color(theme.c(Role::TextPrimary))
        .child(check(theme, r.on))
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.0))
                .child(
                    div()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(SharedString::from(r.name)),
                )
                .child(
                    div()
                        .font_family("Cascadia Mono")
                        .text_size(px(11.5))
                        .text_color(theme.c(Role::TextSecondary))
                        .child(SharedString::from(r.host)),
                ),
        )
        .on_click(cx.listener(move |h, _: &gpui_kit::ClickEvent, w, cx| {
            h.run(
                Command::new("ssh.group.toggle").arg("id", i64::from(id)),
                w,
                cx,
            );
        }))
}

/// Флажок сеанса (`aria`: состав группы): акцентная заливка и галочка.
fn check(theme: &Theme, on: bool) -> gpui_kit::Div {
    let d = div()
        .flex_none()
        .size(px(14.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.0))
        .border_1();
    if on {
        d.border_color(theme.c(Role::AccentFill))
            .bg(theme.c(Role::AccentFill))
            .font_family("Segoe Fluent Icons")
            .text_size(px(10.0))
            .text_color(theme.c(Role::BgBase))
            .child("\u{E73E}")
    } else {
        d.border_color(theme.c(Role::TextSecondary))
            .bg(theme.c(Role::BgBase))
    }
}

/// Блок сеанса: заголовок и строки вывода.
fn block(theme: &Theme, b: &Block, tail: urfin_term::vt::grid::Snapshot, h: f32) -> gpui_kit::Div {
    let code = crate::ssh_group::code(b.exit);
    div()
        .flex_none()
        .flex()
        .flex_col()
        .child(
            div()
                .h(px(H_HEAD))
                .flex_none()
                .mt(px(GAP_HEAD))
                .mx(px(term_view::PAD_X))
                .px(px(8.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .rounded(px(4.0))
                .bg(theme.c(Role::Header))
                .text_size(px(12.0))
                .whitespace_nowrap()
                .child(
                    div()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(theme.c(Role::TextPrimary))
                        .child(SharedString::from(b.name.clone())),
                )
                .child(
                    div()
                        .font_family("Cascadia Mono")
                        .text_size(px(11.5))
                        .text_color(theme.c(Role::TextSecondary))
                        .child(SharedString::from(b.host.clone())),
                )
                .child(div().flex_1())
                .children(code.map(|(text, bad)| {
                    div()
                        .text_color(theme.c(if bad {
                            Role::CriticalFg
                        } else {
                            Role::TextSecondary
                        }))
                        .child(SharedString::from(text))
                })),
        )
        .child(div().h(px(h)).flex_none().child(term_view::view(
            Rc::new(tail),
            theme.m(Metric::TypeMono),
            term_view::GridBounds::default(),
        )))
}

/// Строка «Отправить в 3 сеанса: `<набранное>`? Enter — да, Esc — нет» над вводом.
fn ask_line(theme: &Theme, (lead, typed, tail): (String, String, String)) -> gpui_kit::Div {
    div()
        .h(px(H_ASK))
        .flex_none()
        .flex()
        .items_center()
        .px(px(14.0))
        .bg(theme.c(Role::CautionBg))
        .border_t_2()
        .border_color(theme.c(Role::ContextRemote))
        .text_size(px(13.0))
        .text_color(theme.c(Role::TextPrimary))
        .whitespace_nowrap()
        .overflow_hidden()
        .child(
            div()
                .flex_none()
                .mr(px(6.0))
                .child(SharedString::from(lead)),
        )
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .font_family("Cascadia Mono")
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(SharedString::from(typed)),
        )
        .child(div().flex_none().child(SharedString::from(tail)))
}

/// Сцены группы (`docs/design/screens/screen-sshmulti.png`): три сеанса из четырёх, первая
/// команда подтверждена, у srv3 код 1; строка подтверждения.
pub(super) const SCENES: &[Scene] = &[
    Scene {
        name: "screen-sshmulti",
        steps: &["ui.cancel", "resize 1280x760", "screens.ssh kind=multi"],
    },
    Scene {
        name: "sshmulti-ask",
        steps: &["screens.ssh kind=multi-ask"],
    },
    Scene {
        name: "sshmulti-closed",
        steps: &["screens.ssh kind=clear", "resize 1200x760"],
    },
];
