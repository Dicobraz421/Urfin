//! Токены интерфейса (S2 §1.2–1.3): цвета ролей и метрики, которыми рисуют виды.
//! Цвета выводятся из темы оформления ([`crate::theme`], T-01, №59): роли дизайна
//! (frame, panel, header, …) → роли S2 по таблице в [`palette_of`]; поверх —
//! переопределения `[ui.tokens.light|dark]`.

#![allow(
    clippy::many_single_char_names,
    reason = "цветовые формулы: r, g, b, a"
)]

use std::fmt::Write as _;

use crate::color::Color32;
use urfin_core::ConfigWarning;

use crate::accent::AccentPalette;
use crate::config::UiCfg;
use crate::sys::SysProbe;
use crate::theme::{self, OsLook, Theme, Themes};

macro_rules! roles {
    ($($role:ident = $name:literal,)+) => {
        /// Цветовая роль (S2 §1.3). Имя — ключ `[ui.tokens.light|dark]` и `@роль` в раскраске.
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum Role { $($role,)+ }

        impl Role {
            pub const ALL: &[Role] = &[$(Role::$role,)+];
            pub const COUNT: usize = Role::ALL.len();

            pub fn name(self) -> &'static str {
                match self { $(Role::$role => $name,)+ }
            }

            pub fn from_name(name: &str) -> Option<Role> {
                match name { $($name => Some(Role::$role),)+ _ => None }
            }
        }
    };
}

roles! {
    AccentFill = "accent.fill",
    AccentFillHover = "accent.fill.hover",
    AccentFillPressed = "accent.fill.pressed",
    AccentText = "accent.text",
    AccentOn = "accent.on",
    AccentSubtle = "accent.subtle",
    // Палитра B (решение 2026-11-01): `accent_text` и `accent_soft` темы.
    AccentKey = "accent.key",
    AccentSoft = "accent.soft",
    TextPrimary = "text.primary",
    TextSecondary = "text.secondary",
    TextTertiary = "text.tertiary",
    TextDisabled = "text.disabled",
    BgBase = "bg.base",
    BgBaseAlt = "bg.base.alt",
    LayerPanel = "layer.panel",
    LayerOverlay = "layer.overlay",
    FillSubtleHover = "fill.subtle.hover",
    FillSubtlePressed = "fill.subtle.pressed",
    FillCursor = "fill.cursor",
    FillCursorPassive = "fill.cursor.passive",
    FillControl = "fill.control",
    FillControlInput = "fill.control.input",
    StrokeCard = "stroke.card",
    StrokeControl = "stroke.control",
    StrokeControlBottom = "stroke.control.bottom",
    StrokeStrong = "stroke.strong",
    StrokeDivider = "stroke.divider",
    ShadowOverlay = "shadow.overlay",
    CriticalFg = "critical.fg",
    CriticalBg = "critical.bg",
    CautionFg = "caution.fg",
    CautionBg = "caution.bg",
    SuccessFg = "success.fg",
    SuccessBg = "success.bg",
    InfoBg = "info.bg",
    TypeArchive = "type.archive",
    TypeExec = "type.exec",
    TypeTemp = "type.temp",
    TypeMuted = "type.muted",
    // Роли дизайна без пары в S2 (docs/THEMES.md).
    Header = "header",
    CursorText = "cursor.text",
    CurrentLine = "current_line",
    Highlight = "highlight",
    Modified = "modified",
    ContextAdmin = "context.admin",
    ContextRemote = "context.remote",
}

macro_rules! metrics {
    ($($m:ident = $name:literal : $v:expr,)+) => {
        /// Размер в логических px (S2 §1.1–1.2).
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum Metric { $($m,)+ }

        impl Metric {
            pub const ALL: &[Metric] = &[$(Metric::$m,)+];
            pub const COUNT: usize = Metric::ALL.len();

            pub fn name(self) -> &'static str {
                match self { $(Metric::$m => $name,)+ }
            }

            /// Значение по умолчанию (плотность compact).
            const fn default_value(self) -> f32 {
                match self { $(Metric::$m => $v,)+ }
            }
        }
    };
}

metrics! {
    SpaceXxs = "space.xxs": 2.0,
    SpaceXs = "space.xs": 4.0,
    SpaceS = "space.s": 8.0,
    SpaceM = "space.m": 12.0,
    SpaceL = "space.l": 16.0,
    SpaceXl = "space.xl": 20.0,
    SpaceXxl = "space.xxl": 24.0,
    RadiusControl = "radius.control": 4.0,
    RadiusOverlay = "radius.overlay": 8.0,
    StrokeThin = "stroke.thin": 1.0,
    FocusOuter = "focus.outer": 2.0,
    FocusInner = "focus.inner": 1.0,
    HTitlebar = "h.titlebar": 40.0,
    HTab = "h.tab": 32.0,
    // Метрики окна — docs/design/DESIGN.md (D-02).
    HBreadcrumb = "h.breadcrumb": 30.0,
    HColHeader = "h.col_header": 24.0,
    HRow = "h.row": 24.0,
    HPanelFooter = "h.panel_footer": 28.0,
    HJobRow = "h.job_row": 32.0,
    HBanner = "h.banner": 48.0,
    HCmdline = "h.cmdline": 30.0,
    HKeybar = "h.keybar": 26.0,
    HStatus = "h.status": 26.0,
    // Разделитель панелей (линия `border`).
    WDivider = "w.divider": 1.0,
    HControl = "h.control": 24.0,
    HMenuItem = "h.menu_item": 28.0,
    WIcon = "w.icon": 16.0,
    WPill = "w.pill": 3.0,
    WAccentBar = "w.accent_bar": 3.0,
    GapPanels = "gap.panels": 8.0,
    PadRowX = "pad.row_x": 12.0,
    TypeCaption = "type.caption": 12.0,
    TypeBody = "type.body": 14.0,
    TypeSubtitle = "type.subtitle": 20.0,
    TypeMono = "type.mono": 13.0,
}

/// sRGBA без премультипликации: `Color32` хранит премультиплицированные значения, и
/// полупрозрачные токены WinUI при обратном чтении теряли бы точность.
pub type Rgba = [u8; 4];

/// Цвета всех ролей одной темы.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette([Rgba; Role::COUNT]);

impl Palette {
    pub fn get(&self, r: Role) -> Color32 {
        let [red, g, b, a] = self.0[r as usize];
        Color32::from_rgba_unmultiplied(red, g, b, a)
    }

    /// Значение как задано (для таблиц и сверки).
    pub fn raw(&self, r: Role) -> Rgba {
        self.0[r as usize]
    }

    fn set(&mut self, r: Role, c: Rgba) {
        self.0[r as usize] = c;
    }
}

/// Токены окна: палитра текущей темы и метрики.
#[derive(Clone, Debug, PartialEq)]
pub struct Tokens {
    palette: Palette,
    metrics: [f32; Metric::COUNT],
    /// Тема, из которой выведена палитра (с подтянутым контрастом).
    pub theme: Theme,
    pub high_contrast: bool,
    pub animations: bool,
}

/// Минимальный контраст текста (WCAG AA).
pub const MIN_CONTRAST: f32 = 4.5;

impl Tokens {
    /// Токены из probe ОС и `[ui]`: тема `[ui] theme` среди встроенных, иначе «как в ОС».
    /// Ошибки конфига — предупреждения, берётся дефолт.
    pub fn build(probe: &SysProbe, cfg: &UiCfg) -> (Tokens, Vec<ConfigWarning>) {
        let mut warnings = Vec::new();
        let os = os_look(probe, cfg, &mut warnings);
        let theme = Themes::default().resolve(&theme::normalize_choice(&cfg.theme), &os);
        let (t, w) = Self::from_theme(theme, cfg);
        warnings.extend(w);
        (t, warnings)
    }

    /// Токены из готовой темы: палитра по таблице ролей, переопределения, метрики.
    pub fn from_theme(theme: Theme, cfg: &UiCfg) -> (Tokens, Vec<ConfigWarning>) {
        let mut warnings = Vec::new();
        let mut palette = palette_of(&theme);
        let hc = theme.id == "high-contrast";
        if hc {
            // Только системные цвета: без полупрозрачности и без переопределений.
            for r in Role::ALL {
                let [red, g, b, a] = palette.raw(*r);
                if a != 255 && a != 0 {
                    let solid = matches!(r, Role::AccentFillHover | Role::AccentFillPressed);
                    palette.set(*r, [red, g, b, if solid { 255 } else { 0 }]);
                }
            }
        }
        let none = std::collections::BTreeMap::new();
        let (section, overrides) = if hc {
            ("", &none)
        } else if theme.dark {
            ("ui.tokens.dark", &cfg.tokens.dark)
        } else {
            ("ui.tokens.light", &cfg.tokens.light)
        };
        for (name, value) in overrides {
            let Some(role) = Role::from_name(name) else {
                warnings.push(warn(section, format!("unknown token {name:?}")));
                continue;
            };
            match parse_rgba(value) {
                Some(c) => palette.set(role, c),
                None => warnings.push(warn(section, format!("{name}: bad color {value:?}"))),
            }
        }
        let mut metrics: [f32; Metric::COUNT] =
            std::array::from_fn(|i| Metric::ALL[i].default_value());
        metrics[Metric::HRow as usize] = f32::from(cfg.density.px());
        let body = cfg.font_ui_size.clamp(8.0, 32.0);
        metrics[Metric::TypeBody as usize] = body;
        metrics[Metric::TypeCaption as usize] = body - 1.0;
        metrics[Metric::TypeSubtitle as usize] = body + 6.0;
        metrics[Metric::TypeMono as usize] = cfg.font_mono_size.clamp(8.0, 32.0);
        let tokens = Tokens {
            palette,
            metrics,
            high_contrast: theme.id == "high-contrast",
            theme,
            animations: cfg.animations,
        };
        (tokens, warnings)
    }

    /// Цвет роли.
    pub fn c(&self, r: Role) -> Color32 {
        self.palette.get(r)
    }

    pub fn m(&self, m: Metric) -> f32 {
        self.metrics[m as usize]
    }

    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// Тёмная тема (иконки, системные контролы).
    pub fn dark(&self) -> bool {
        self.theme.dark
    }
}

impl Default for Tokens {
    fn default() -> Self {
        Self::build(&SysProbe::default(), &UiCfg::default()).0
    }
}

fn warn(key: &str, msg: String) -> ConfigWarning {
    ConfigWarning {
        key: key.to_owned(),
        msg,
    }
}

/// Что тема берёт от ОС: светлая/тёмная, акцент (`[ui] accent` — свой), контрастность.
pub fn os_look(probe: &SysProbe, cfg: &UiCfg, warnings: &mut Vec<ConfigWarning>) -> OsLook {
    let accent = match cfg.accent.trim() {
        a if a.is_empty() || a.eq_ignore_ascii_case("system") => probe.look.accent,
        a => {
            if let Some(c) = parse_color(a) {
                AccentPalette::from_base(c.to_opaque())
            } else {
                warnings.push(warn("ui.accent", format!("bad color {a:?}")));
                probe.look.accent
            }
        }
    };
    OsLook {
        dark: probe.look.theme == Some(crate::color::Theme::Dark),
        accent: Some(accent),
        high_contrast: probe.look.high_contrast.then_some(probe.look.hc).flatten(),
    }
}

/// Роли S2 из ролей темы. Таблица соответствия:
///
/// | S2 | тема |
/// |---|---|
/// | `bg.base` / `bg.base.alt` | `frame` / `header` |
/// | `layer.panel`, `layer.overlay`, `fill.control*` | `panel` |
/// | `text.primary` / `.secondary` / `.tertiary` | `text` / `secondary` / `secondary` |
/// | `text.disabled` | `secondary` на 60 % к `panel` |
/// | `accent.fill` (+ hover 90 %, pressed 80 %) | `accent` |
/// | `accent.on` | белый или чёрный — где контраст с `accent` выше |
/// | `accent.subtle` | `accent` 12 % (светлая) / 16 % (тёмная) |
/// | `accent.text` | `accent`, подтянутый до 4,5:1 к `accent.subtle` над `panel` (отмеченные) |
/// | `accent.key` / `accent.soft` | `accent_text` / `accent_soft` (палитра B: клавиши keybar и части чипов на `frame`/`header`; тон активной панели, шапка таблицы Markdown) |
/// | `fill.cursor` / `.passive` | `cursor` / `cursor` 50 % |
/// | `fill.subtle.*` | `text` 4 % / 6 % |
/// | `stroke.card/control/divider` | `border` |
/// | `stroke.control.bottom`, `stroke.strong`, `type.muted` | `secondary` |
/// | `critical/caution/success.fg` | `error` / `warning` / `success` |
/// | `*.bg` статусов | тот же цвет 14 % к `panel` |
/// | `info.bg` | `header` |
/// | `type.archive` / `type.exec` / `type.temp` | `archive` / `exec` / `warning` |
/// | `header`, `cursor.text`, `current_line`, `highlight`, `modified`, `context.*` | одноимённые |
pub fn palette_of(t: &Theme) -> Palette {
    let r = &t.roles;
    let mut p = Palette([[0; 4]; Role::COUNT]);
    let solid = |c: Color32| rgba(c);
    let tint = |c: Color32, k: f32| rgba(theme::mix(r.panel, c, k));
    p.set(Role::BgBase, solid(r.frame));
    p.set(Role::BgBaseAlt, solid(r.header));
    for role in [
        Role::LayerPanel,
        Role::LayerOverlay,
        Role::FillControl,
        Role::FillControlInput,
    ] {
        p.set(role, solid(r.panel));
    }
    p.set(Role::TextPrimary, solid(r.text));
    p.set(Role::TextSecondary, solid(r.secondary));
    p.set(Role::TextTertiary, solid(r.secondary));
    p.set(Role::TextDisabled, tint(r.secondary, 0.6));
    p.set(Role::AccentFill, solid(r.accent));
    p.set(Role::AccentFillHover, with_alpha(r.accent, 0.9));
    p.set(Role::AccentFillPressed, with_alpha(r.accent, 0.8));
    p.set(Role::AccentOn, rgba(best_on(r.accent)));
    let subtle = with_alpha(r.accent, if t.dark { 0.16 } else { 0.12 });
    p.set(Role::AccentSubtle, subtle);
    let marked_bg = over(p.get(Role::AccentSubtle), r.panel);
    p.set(
        Role::AccentText,
        rgba(theme::toward_contrast(r.accent, marked_bg, t.dark)),
    );
    p.set(Role::AccentKey, solid(r.accent_text));
    p.set(Role::AccentSoft, solid(r.accent_soft));
    p.set(Role::FillCursor, solid(r.cursor));
    p.set(Role::FillCursorPassive, with_alpha(r.cursor, 0.5));
    p.set(Role::FillSubtleHover, with_alpha(r.text, 0.04));
    p.set(Role::FillSubtlePressed, with_alpha(r.text, 0.06));
    for role in [Role::StrokeCard, Role::StrokeControl, Role::StrokeDivider] {
        p.set(role, solid(r.border));
    }
    for role in [
        Role::StrokeControlBottom,
        Role::StrokeStrong,
        Role::TypeMuted,
    ] {
        p.set(role, solid(r.secondary));
    }
    p.set(
        Role::ShadowOverlay,
        with_alpha(Color32::BLACK, if t.dark { 0.26 } else { 0.14 }),
    );
    p.set(Role::CriticalFg, solid(r.error));
    p.set(Role::CriticalBg, tint(r.error, 0.14));
    p.set(Role::CautionFg, solid(r.warning));
    p.set(Role::CautionBg, tint(r.warning, 0.14));
    p.set(Role::SuccessFg, solid(r.success));
    p.set(Role::SuccessBg, tint(r.success, 0.14));
    p.set(Role::InfoBg, solid(r.header));
    p.set(Role::TypeArchive, solid(r.archive));
    p.set(Role::TypeExec, solid(r.exec));
    p.set(Role::TypeTemp, solid(r.warning));
    p.set(Role::Header, solid(r.header));
    p.set(Role::CursorText, solid(r.cursor_text));
    p.set(Role::CurrentLine, solid(r.current_line));
    p.set(Role::Highlight, solid(r.highlight));
    p.set(Role::Modified, solid(r.modified));
    p.set(Role::ContextAdmin, solid(r.context_admin));
    p.set(Role::ContextRemote, solid(r.context_remote));
    p
}

/// `#RRGGBB` или `#AARRGGBB`.
pub fn parse_color(s: &str) -> Option<Color32> {
    parse_rgba(s).map(|[r, g, b, a]| Color32::from_rgba_unmultiplied(r, g, b, a))
}

fn parse_rgba(s: &str) -> Option<Rgba> {
    let hex = s.strip_prefix('#')?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    match hex.len() {
        6 => Some(argb(0xFF00_0000 | v)),
        8 => Some(argb(v)),
        _ => None,
    }
}

/// `0xAARRGGBB` → sRGBA.
pub(crate) const fn argb(v: u32) -> Rgba {
    let [a, r, g, b] = v.to_be_bytes();
    [r, g, b, a]
}

/// Непрозрачный цвет → sRGBA.
fn rgba(c: Color32) -> Rgba {
    c.to_srgba_unmultiplied()
}

/// Непрозрачный цвет с долей непрозрачности.
#[allow(clippy::cast_sign_loss, reason = "доля зажата в 0..=1")]
fn with_alpha(c: Color32, alpha: f32) -> Rgba {
    let [r, g, b, _] = c.to_srgba_unmultiplied();
    [r, g, b, (255.0 * alpha.clamp(0.0, 1.0)).round() as u8]
}

/// `fg` поверх непрозрачного `bg` (альфа-смешение в sRGB, как у композитора).
#[allow(clippy::cast_sign_loss, reason = "значения 0..=255")]
pub fn over(fg: Color32, bg: Color32) -> Color32 {
    let [r, g, b, a] = fg.to_srgba_unmultiplied();
    let k = f32::from(a) / 255.0;
    let mix = |f: u8, b: u8| (f32::from(f) * k + f32::from(b) * (1.0 - k)).round() as u8;
    Color32::from_rgb(mix(r, bg.r()), mix(g, bg.g()), mix(b, bg.b()))
}

/// Относительная яркость WCAG.
fn luminance(c: Color32) -> f32 {
    let lin = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// Контраст WCAG двух непрозрачных цветов.
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Текст на акценте: белый или чёрный — где контраст выше (минимум ≈ 4,58 для любого цвета).
fn best_on(fill: Color32) -> Color32 {
    let fill = fill.to_opaque();
    if contrast(Color32::WHITE, fill) >= contrast(Color32::BLACK, fill) {
        Color32::WHITE
    } else {
        Color32::BLACK
    }
}

/// Таблица `роль  цвет` темы и метрики (insta, документация).
pub fn table(t: &Tokens) -> String {
    let hex = |[r, g, b, a]: Rgba| {
        if a == 255 {
            format!("#{r:02X}{g:02X}{b:02X}")
        } else {
            format!("#{a:02X}{r:02X}{g:02X}{b:02X}")
        }
    };
    let mut out = String::new();
    for r in Role::ALL {
        let _ = writeln!(out, "{:<22} {}", r.name(), hex(t.palette().raw(*r)));
    }
    for m in Metric::ALL {
        let _ = writeln!(out, "{:<22} {}", m.name(), t.m(*m));
    }
    out
}

#[cfg(test)]
mod tests;
