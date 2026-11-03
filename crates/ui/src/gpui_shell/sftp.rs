//! Панель `sftp://` в окне GPUI (F03-06, F03 §2.3): строка обрыва связи внизу панели
//! (`screen-sftplost`) — «Связь с srv1 потеряна · переподключение через N с (попытка k
//! из M)», «Переподключить сейчас» / «Отключиться»; после исчерпания попыток —
//! «Отключено» и «Переподключить». Enter у таба из сессии без связи — «Подключить».
//! Тексты и команды — `crate::ssh` (без gpui); заголовок, приглушение и «Не подключено»
//! рисует панель.

use std::time::{Duration, Instant};

use gpui_kit::prelude::*;
use gpui_kit::{Context, Div, Keystroke, SharedString, div, px};
use urfin_core::{AppState, Command};

use super::CoreHost;
use super::features::{Frame, ShellFeature};
use super::screens::Scene;
use super::strip::{Btn, Tone, button, container};
use crate::ssh::{Link, LostRow};
use crate::tokens::Role;

/// Высота строки обрыва: поля 10, строка 20, зазор 8, кнопки 30, черта 2.
pub(super) const H_LOST: f32 = 80.0;
/// «Нет сети» (Segoe Fluent Icons `NetworkOffline`).
const GLYPH_OFFLINE: &str = "\u{F384}";

/// Фича окна: строка обрыва и клавиши панели `sftp://` без связи.
#[derive(Default)]
pub(super) struct SftpPanel {
    /// Кнопка под Tab в строке обрыва.
    btn: usize,
    /// Для какой строки выбрана кнопка: новая строка — снова первая.
    row: Option<(urfin_core::Side, String, bool)>,
}

impl SftpPanel {
    /// Tab в строке `r` уже нажимали.
    fn touched(&self, r: &LostRow) -> bool {
        self.row.as_ref().is_some_and(|(side, p, lost)| {
            *side == r.side && *p == r.profile && *lost == matches!(r.link, Link::Lost { .. })
        })
    }

    /// Кнопка под Tab для строки `r` (другая строка — первая кнопка).
    fn btn_for(&self, r: &LostRow) -> usize {
        if self.touched(r) { self.btn } else { 0 }
    }

    fn remember(&mut self, r: &LostRow, btn: usize) {
        self.row = Some((
            r.side,
            r.profile.clone(),
            matches!(r.link, Link::Lost { .. }),
        ));
        self.btn = btn;
    }
}

/// Нажатие в панелях без модификаторов (Shift — только у Tab), когда клавиши не ждут
/// вопрос, меню, черновик, переименование, быстрый поиск и командная строка (Enter с
/// набранной командой — её).
fn plain(s: &AppState, ks: &Keystroke) -> bool {
    let m = &ks.modifiers;
    let busy = s.ext.get::<crate::UiState>().is_some_and(|u| {
        u.draft.is_some() || u.menu.is_some() || u.rename.is_some() || u.settings.is_some()
    }) || s
        .ext
        .get::<urfin_keymap::KeymapView>()
        .is_some_and(|k| k.qs.is_some() || k.pending.is_some());
    !m.control
        && !m.alt
        && !m.platform
        && s.focus == urfin_core::Focus::Panel
        && s.prompts.active().is_none()
        && !busy
        && urfin_term::cmdline_empty(s)
}

impl ShellFeature for SftpPanel {
    /// Отсчёт «переподключение через N с» — перерисовка к смене секунды.
    fn tick(&mut self, s: &AppState, now: Instant) -> Option<Duration> {
        let r = crate::ssh::lost_row(s)?;
        let Link::Lost {
            retry_at: Some(at), ..
        } = r.link
        else {
            return None;
        };
        let left = at.checked_duration_since(now)?;
        let frac = left.as_millis() % 1000;
        Some(Duration::from_millis(u64::try_from(frac).unwrap_or(0) + 5))
    }

    fn key(&mut self, s: &AppState, ks: &Keystroke) -> Option<Vec<Command>> {
        if !plain(s, ks) {
            return None;
        }
        if let Some(r) = crate::ssh::lost_row(s).filter(|r| r.side == s.active) {
            let buttons = crate::ssh::lost_buttons(&r);
            let btn = self.btn_for(&r).min(buttons.len().saturating_sub(1));
            return match ks.key.as_str() {
                "tab" if buttons.len() > 1 => {
                    let n = buttons.len();
                    let next = if ks.modifiers.shift {
                        (btn + n - 1) % n
                    } else {
                        (btn + 1) % n
                    };
                    self.remember(&r, next);
                    Some(Vec::new())
                }
                "enter" if !ks.modifiers.shift => {
                    self.row = None;
                    self.btn = 0;
                    buttons.into_iter().nth(btn).map(|(_, _, cmd)| vec![cmd])
                }
                _ => None,
            };
        }
        let t = s.active_tab();
        if ks.key == "enter" && !ks.modifiers.shift && crate::ssh::offline(s, t) {
            return crate::ssh::connect_cmd(t, s.active).map(|c| vec![c]);
        }
        None
    }

    fn strip(&self, f: &Frame<'_>, _cx: &Context<CoreHost>) -> Option<Div> {
        let r = crate::ssh::lost_row(f.s)?;
        Some(self.row_el(&r, f))
    }
}

impl SftpPanel {
    /// Строка `warning_bg` (`screen-sftplost`): значок, «Связь с srv1 потеряна» и отсчёт,
    /// ниже — кнопки; выбранная Tab — в рамке `accent`.
    fn row_el(&self, r: &LostRow, f: &Frame<'_>) -> Div {
        let theme = f.theme;
        let (title, note) = crate::ssh::lost_text(r, Instant::now());
        let side = super::chrome::side_name(r.side);
        // Рамка `accent` — после Tab (как на макете, до него без рамки).
        let sel = (r.side == f.s.active && self.touched(r)).then(|| self.btn_for(r));
        let mut buttons = div().flex().items_center().gap(px(8.0));
        for (i, (label, key, cmd)) in crate::ssh::lost_buttons(r).into_iter().enumerate() {
            let b = button(
                theme,
                format!("sftp-lost-{side}-{i}"),
                label,
                key,
                Btn::Normal,
            )
            .when(sel == Some(i), |d| {
                d.border_color(theme.c(Role::AccentFill))
            })
            .on_click((f.click_all)(vec![
                Command::new("panel.focus").arg("side", side),
                cmd,
            ]));
            buttons = buttons.child(b);
        }
        container(theme, Tone::Waiting, 14.0, 10.0)
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .h(px(20.0))
                    .child(
                        div()
                            .flex_none()
                            .font_family("Segoe Fluent Icons")
                            .text_size(px(14.0))
                            .text_color(theme.c(Role::ContextRemote))
                            .child(SharedString::new_static(GLYPH_OFFLINE)),
                    )
                    .child(
                        div()
                            .flex()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(
                                div()
                                    .flex_none()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(theme.c(Role::TextPrimary))
                                    .child(SharedString::from(title)),
                            )
                            .child(
                                div()
                                    .text_color(theme.c(Role::TextSecondary))
                                    .child(SharedString::from(format!(" {note}"))),
                            ),
                    ),
            )
            .child(buttons)
    }
}

/// Сцены панели `sftp://` (`docs/design/screens/screen-sftp*.png`): фикстура
/// `screens.ssh kind=sftp|lost`, папка — backend сцен (`urfin_net::sftp::demo`), без сети.
pub(super) const SCENES: &[Scene] = &[
    // Переход отдельной сценой: снимок — когда листинг `sftp://` уже на экране.
    Scene {
        name: "sftppanel-open",
        steps: &[
            "ui.cancel",
            "resize 1280x760",
            "screens.ssh kind=sftp",
            "panel.go side=right path=sftp://srv1/var/www/app",
            "panel.focus side=right",
        ],
    },
    Scene {
        name: "screen-sftppanel",
        steps: &["panel.cursor.to name=current"],
    },
    Scene {
        name: "screen-sftplost",
        steps: &["screens.ssh kind=lost"],
    },
    // Попытки исчерпаны: «Отключено от srv1», «Переподключить».
    Scene {
        name: "sftp-off",
        steps: &["screens.ssh kind=off"],
    },
    // F03-07: F5 на сервер (`screen-sftpcopy`) — слева `dist` демо-дерева, справа
    // `releases`; строка копирования с «Сохранять права и даты», задачи с адресами
    // `sftp://` и докачкой.
    Scene {
        name: "sftpcopy-open",
        steps: &[
            "screens.ssh kind=sftp",
            "panel.go side=right path=sftp://srv1/var/www/app/releases",
            "panel.focus side=left",
            "panel.cursor.to name=Документы",
            "panel.enter",
        ],
    },
    // Каждый переход — своей сценой: следующая ждёт листинга.
    Scene {
        name: "sftpcopy-dist",
        steps: &["panel.cursor.to name=dist", "panel.enter"],
    },
    Scene {
        name: "sftpcopy-marks",
        steps: &[
            "panel.cursor.to name=assets",
            "sel.toggle",
            "panel.cursor.to name=app.4f2a91c.js",
            "sel.toggle",
            "panel.cursor.to name=index.html",
            "sel.toggle",
            "panel.cursor.to name=index.html",
        ],
    },
    Scene {
        name: "screen-sftpcopy",
        steps: &["screens.jobs kind=sftp", "ops.copy"],
    },
    Scene {
        name: "sftpcopy-closed",
        steps: &["ui.cancel", "screens.jobs stop=true", "sel.none"],
    },
    // Сравнение и синхронизация с сервером (`screen-sftpsync`): стрелки у того, что
    // уйдёт, «только здесь» на сервере, «на сервере лишних», «Удалять лишнее» снят.
    Scene {
        name: "sftpsync-open",
        steps: &[
            "panel.go side=right path=sftp://srv1/var/www/app/current",
            "panel.cursor.to name=index.html",
        ],
    },
    // Сравнение — по листингам на экране: после перехода, отдельной сценой.
    Scene {
        name: "screen-sftpsync",
        steps: &["ops.sync dir=lr"],
    },
    Scene {
        name: "sftpsync-closed",
        steps: &[
            "ui.cancel",
            "sel.none",
            "panel.focus side=left",
            "panel.back",
            "panel.back",
            "panel.focus side=right",
            "panel.back",
            "panel.back",
        ],
    },
    Scene {
        name: "sftp-closed",
        steps: &[
            "screens.ssh kind=clear",
            "panel.back",
            "panel.focus side=left",
            "resize 1200x760",
        ],
    },
];
