//! Приветствие (D-02) как фича окна (ADR-0022): выбор клавиш Far/DN и темы «как в
//! Windows» до подтверждения; строка — `strip::welcome_bar`.

use gpui_kit::{ClickEvent, Context, Div, Keystroke};
use urfin_core::{AppState, Command};

use super::CoreHost;
use super::features::{Frame, ShellFeature};

/// Выбор в приветствии: клавиши DN (иначе Far), тема как в Windows.
pub(super) struct Welcome {
    dn: bool,
    follow: bool,
}

impl Default for Welcome {
    fn default() -> Welcome {
        Welcome {
            dn: false,
            follow: true,
        }
    }
}

impl ShellFeature for Welcome {
    /// ← → — Far/DN, Пробел — тема, Enter — готово, Esc — позже.
    fn key(&mut self, s: &AppState, ks: &Keystroke) -> Option<Vec<Command>> {
        let w = s.ext.get::<crate::UiState>()?.welcome.as_ref()?;
        if s.prompts.active().is_some() {
            return None;
        }
        let first = matches!(w, crate::state::Welcome::First);
        let cmd = match ks.key.as_str() {
            "left" | "right" if first => {
                self.dn = !self.dn;
                None
            }
            "space" if first => {
                self.follow = !self.follow;
                None
            }
            "enter" if first => {
                let mut c = Command::new("ui.welcome.done")
                    .arg("preset", if self.dn { "dn" } else { "far" });
                if self.follow {
                    c = c.arg("follow_os", true);
                }
                Some(c)
            }
            "enter" | "escape" => Some(Command::new("ui.welcome.done")),
            _ => return None,
        };
        Some(cmd.into_iter().collect())
    }

    fn strip(&self, f: &Frame<'_>, cx: &Context<CoreHost>) -> Option<Div> {
        let w = f.s.ext.get::<crate::UiState>()?.welcome.as_ref()?;
        let pick = |what: u8| -> super::overlays::Handler {
            Box::new(cx.listener(move |h: &mut CoreHost, _: &ClickEvent, _, cx| {
                if let Some(me) = h.features.get_mut::<Welcome>() {
                    match what {
                        0 => me.dn = false,
                        1 => me.dn = true,
                        _ => me.follow = !me.follow,
                    }
                }
                cx.notify();
            }))
        };
        Some(super::strip::welcome_bar(
            w,
            self.dn,
            self.follow,
            f.theme,
            f.click_all,
            &pick,
        ))
    }
}
