//! Тема окна GPUI: токены из темы оформления (T-01, №59), раскраска файлов
//! ([`Highlighter`]) и формат ячеек ([`FmtCfg`]).
//!
//! Токены пересобираются, когда меняется что-то из входа: выбор темы (`ui.theme.*`),
//! светлая/тёмная ОС (gpui следит за окном), акцент и контрастность (`sys::watch`),
//! свои темы (`ui.theme.reload`), высота строки. Номер пересборки — [`Theme::generation`],
//! он входит в отпечатки кэшируемых видов.

use crate::color::Color32;
use gpui_kit::{Hsla, Rgba};

use crate::config::UiCfg;
use crate::fmt_cell::FmtCfg;
use crate::highlight::Highlighter;
use crate::state::UserThemes;
use crate::sys::{Look, SysProbe, Zone};
use crate::theme::{OsLook, Themes};
use crate::tokens::{Metric, Role, Tokens, os_look};

/// Всё, что нужно видам для цвета, размеров и текста ячеек.
pub struct Theme {
    pub tokens: Tokens,
    pub hl: Highlighter,
    pub fmt: FmtCfg,
    cfg: UiCfg,
    os: OsLook,
    /// Вход последней сборки: выбор, тёмная ОС, поколение своих тем, высота строки.
    key: (String, bool, u64, u8),
    user: Themes,
    generation: u64,
    /// «Эффекты анимации» Windows выключены (`SPI_GETCLIENTAREAANIMATION`).
    reduced_motion: bool,
    /// Снимки `--screens`: кадр не зависит от времени.
    still: bool,
}

impl std::fmt::Debug for Theme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Theme")
            .field("id", &self.tokens.theme.id)
            .finish_non_exhaustive()
    }
}

impl Theme {
    pub fn new(probe: &SysProbe, cfg: &UiCfg) -> Theme {
        let (tokens, _) = Tokens::build(probe, cfg);
        let (hl, _) = Highlighter::build(&cfg.colors);
        let iso = cfg.date_format.eq_ignore_ascii_case("iso");
        let fmt = FmtCfg::new(&urfin_i18n::current(), iso, Zone::system());
        let os = os_look(probe, cfg, &mut Vec::new());
        Theme {
            key: (
                crate::theme::normalize_choice(&cfg.theme),
                os.dark,
                0,
                cfg.density.px(),
            ),
            tokens,
            hl,
            fmt,
            cfg: cfg.clone(),
            os,
            user: Themes::default(),
            generation: 0,
            reduced_motion: probe.look.reduced_motion,
            still: false,
        }
    }

    /// Снимки `--screens`: без движения (блик, наведение) — кадр повторяем.
    pub fn set_still(&mut self) {
        self.still = true;
    }

    /// Анимации (DESIGN «Движение», №72): `ui.animations` и «Эффекты анимации» Windows.
    /// Нет — длительности 0, блик прогресса не бежит.
    pub fn motion(&self) -> bool {
        self.cfg.animations && !self.reduced_motion && !self.still
    }

    /// Палитра B (решение 2026-11-01): тона, акценты и цветные значки; в высокой
    /// контрастности не действует.
    pub fn vivid(&self) -> bool {
        !self.tokens.high_contrast
    }

    /// Акцент, контрастность и светлость ОС из `sys::watch`.
    pub fn set_look(&mut self, look: &Look) {
        self.reduced_motion = look.reduced_motion;
        let probe = SysProbe {
            look: *look,
            ..SysProbe::default()
        };
        let os = os_look(&probe, &self.cfg, &mut Vec::new());
        if os != self.os {
            self.os = os;
            self.rebuild();
        }
    }

    /// Свести тему к входу кадра; `true` — токены пересобраны.
    pub fn sync(&mut self, choice: &str, os_dark: bool, user: &UserThemes, density: u8) -> bool {
        let key = (choice.to_owned(), os_dark, user.generation, density);
        if key == self.key {
            return false;
        }
        if key.2 != self.key.2 {
            self.user = Themes {
                user: user.themes.to_vec(),
            };
        }
        self.key = key;
        self.os.dark = os_dark;
        self.rebuild();
        true
    }

    fn rebuild(&mut self) {
        let theme = self.user.resolve(&self.key.0, &self.os);
        let mut cfg = self.cfg.clone();
        cfg.density = crate::config::Density::new(i64::from(self.key.3));
        self.tokens = Tokens::from_theme(theme, &cfg).0;
        self.generation += 1;
    }

    /// Номер сборки токенов: меняется с темой (ключ кэшей видов).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Цвет роли.
    pub fn c(&self, r: Role) -> Hsla {
        hsla(self.tokens.c(r))
    }

    /// Метрика в px.
    pub fn m(&self, m: Metric) -> f32 {
        self.tokens.m(m)
    }
}

/// `Color32` (sRGB, премультиплицированный) → цвет gpui.
pub fn hsla(color: Color32) -> Hsla {
    let [red, green, blue, alpha] = color.to_srgba_unmultiplied();
    Rgba {
        r: f32::from(red) / 255.0,
        g: f32::from(green) / 255.0,
        b: f32::from(blue) / 255.0,
        a: f32::from(alpha) / 255.0,
    }
    .into()
}
