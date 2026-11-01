use crate::color::Color32;

use super::*;
use crate::sys::{HcColors, Look};

fn probe(accent: AccentPalette, dark: bool) -> SysProbe {
    SysProbe {
        look: Look {
            accent,
            theme: Some(if dark {
                crate::color::Theme::Dark
            } else {
                crate::color::Theme::Light
            }),
            ..Look::default()
        },
        ..SysProbe::default()
    }
}

/// Акценты Windows из «Параметров» (верхний ряд по тонам + серый): синий по умолчанию,
/// жёлтое золото, оранжевый, красный, фиолетовый, бирюзовый, зелёный, серый.
const ACCENTS: [u32; 8] = [
    0x00_78D4, 0xFF_B900, 0xF7_630C, 0xE8_1123, 0x88_1798, 0x00_99BC, 0x10_893E, 0x76_7676,
];

fn rgb(v: u32) -> Color32 {
    let [_, r, g, b] = v.to_be_bytes();
    Color32::from_rgb(r, g, b)
}

fn with_theme(id: &str) -> UiCfg {
    UiCfg {
        theme: id.into(),
        ..UiCfg::default()
    }
}

#[test]
fn every_role_resolves_in_every_theme() {
    for t in crate::theme::builtin() {
        let (tk, w) = Tokens::from_theme(t.clone(), &UiCfg::default());
        assert_eq!(w.len(), 0);
        for r in Role::ALL {
            assert_ne!(tk.palette().raw(*r)[3], 0, "{} {}", t.id, r.name());
        }
    }
    for r in Role::ALL {
        assert_eq!(Role::from_name(r.name()), Some(*r));
    }
}

/// Роли дизайна доходят до токенов как есть (THEMES.md).
#[test]
fn design_roles_map_to_tokens() {
    let (t, _) = Tokens::build(&SysProbe::default(), &with_theme("sepia"));
    let r = crate::theme::builtin_by_id("sepia").unwrap().roles;
    assert_eq!(t.theme.id, "sepia");
    assert_eq!(t.c(Role::BgBase), r.frame);
    assert_eq!(t.c(Role::LayerPanel), r.panel);
    assert_eq!(t.c(Role::Header), r.header);
    assert_eq!(t.c(Role::TextPrimary), r.text);
    assert_eq!(t.c(Role::AccentFill), r.accent);
    assert_eq!(t.c(Role::FillCursor), r.cursor);
    assert_eq!(t.c(Role::StrokeDivider), r.border);
    assert_eq!(t.c(Role::Highlight), r.highlight);
    assert_eq!(t.c(Role::TypeArchive), r.archive);
    assert_eq!(t.c(Role::ContextRemote), r.context_remote);
    assert!(!t.dark());
    let (d, _) = Tokens::build(&SysProbe::default(), &with_theme("dark"));
    assert_eq!(d.theme.id, "system-dark", "S2 value `dark`");
    assert!(d.dark());
}

#[test]
fn contrast_for_system_accents() {
    for v in ACCENTS {
        for dark in [false, true] {
            let (t, _) = Tokens::build(
                &probe(AccentPalette::from_base(rgb(v)), dark),
                &UiCfg::default(),
            );
            let p = t.palette();
            let panel = p.get(Role::LayerPanel);
            let fill = p.get(Role::AccentFill).to_opaque();
            let checks = [
                ("accent.on/accent.fill", p.get(Role::AccentOn), fill),
                ("text.primary/layer.panel", p.get(Role::TextPrimary), panel),
                (
                    "accent.text/accent.subtle",
                    p.get(Role::AccentText),
                    over(p.get(Role::AccentSubtle), panel),
                ),
                (
                    "cursor.text/fill.cursor",
                    p.get(Role::CursorText),
                    p.get(Role::FillCursor),
                ),
                // Палитра B: клавиши keybar на `frame`/`header`, текст на тоне панели.
                (
                    "accent.key/bg.base",
                    p.get(Role::AccentKey),
                    p.get(Role::BgBase),
                ),
                (
                    "accent.key/header",
                    p.get(Role::AccentKey),
                    p.get(Role::Header),
                ),
                (
                    "text.primary/accent.soft",
                    p.get(Role::TextPrimary),
                    p.get(Role::AccentSoft),
                ),
                (
                    "text.secondary/accent.soft",
                    p.get(Role::TextSecondary),
                    p.get(Role::AccentSoft),
                ),
            ];
            for (what, fg, bg) in checks {
                let k = contrast(fg, bg);
                assert!(k >= MIN_CONTRAST, "#{v:06X} dark={dark} {what}: {k:.2}");
            }
        }
    }
}

#[test]
fn overrides_and_bad_values() {
    let mut cfg = with_theme("system-light");
    cfg.tokens
        .light
        .insert("accent.fill".into(), "#0067C0".into());
    cfg.tokens
        .light
        .insert("layer.panel".into(), "#80FFFFFF".into());
    cfg.tokens.light.insert("no.such".into(), "#000000".into());
    cfg.tokens
        .light
        .insert("text.primary".into(), "white".into());
    cfg.tokens.dark.insert("header".into(), "#000000".into());
    let (t, warnings) = Tokens::build(&SysProbe::default(), &cfg);
    assert_eq!(t.c(Role::AccentFill), Color32::from_rgb(0x00, 0x67, 0xC0));
    assert_eq!(
        t.c(Role::LayerPanel),
        Color32::from_rgba_unmultiplied(255, 255, 255, 0x80)
    );
    assert_ne!(
        t.c(Role::Header),
        Color32::BLACK,
        "dark overrides — only in a dark theme"
    );
    let keys: Vec<_> = warnings
        .iter()
        .map(|w| (w.key.as_str(), w.msg.as_str()))
        .collect();
    assert_eq!(
        keys,
        [
            ("ui.tokens.light", "unknown token \"no.such\""),
            ("ui.tokens.light", "text.primary: bad color \"white\""),
        ]
    );
    // Свой акцент ОС-режима: из `[ui] accent`.
    let mut cfg = UiCfg {
        accent: "#E81123".into(),
        ..UiCfg::default()
    };
    let (t, _) = Tokens::build(&SysProbe::default(), &cfg);
    assert_eq!(
        t.c(Role::AccentFill),
        AccentPalette::from_base(rgb(0xE8_1123)).dark1
    );
    cfg.accent = "blue".into();
    let (_, warnings) = Tokens::build(&SysProbe::default(), &cfg);
    assert_eq!(warnings[0].key, "ui.accent");
}

#[test]
fn density_and_font_sizes() {
    let cfg = UiCfg {
        density: crate::config::Density::new(28),
        font_ui_size: 16.0,
        ..UiCfg::default()
    };
    let (t, _) = Tokens::build(&SysProbe::default(), &cfg);
    assert_eq!(t.m(Metric::HRow), 28.0);
    assert_eq!(t.m(Metric::TypeBody), 16.0);
    assert_eq!(t.m(Metric::TypeCaption), 15.0);
    assert_eq!(Tokens::default().m(Metric::HRow), 24.0);
}

#[test]
fn high_contrast_uses_system_colors() {
    let hc = HcColors {
        window: Color32::BLACK,
        window_text: Color32::WHITE,
        highlight: Color32::from_rgb(0x1A, 0xEB, 0xFF),
        highlight_text: Color32::BLACK,
        gray_text: Color32::from_rgb(0x3F, 0xF2, 0x3F),
        hotlight: Color32::from_rgb(0xFF, 0xFF, 0x00),
    };
    let mut p = SysProbe::default();
    p.look.high_contrast = true;
    p.look.hc = Some(hc);
    let mut cfg = with_theme("sepia");
    cfg.tokens
        .dark
        .insert("text.primary".into(), "#123456".into());
    let (t, _) = Tokens::build(&p, &cfg);
    assert!(t.high_contrast);
    assert_eq!(
        t.c(Role::TextPrimary),
        Color32::WHITE,
        "переопределения не действуют"
    );
    assert_eq!(t.c(Role::LayerPanel), Color32::BLACK);
    assert_eq!(t.c(Role::AccentFill), hc.highlight);
    assert_eq!(t.c(Role::AccentOn), Color32::BLACK);
    for r in Role::ALL {
        let a = t.c(*r).a();
        assert!(a == 0 || a == 255, "без полупрозрачности: {}", r.name());
    }
}

/// Таблица токенов «как в ОС» для акцента `#0078D4` (сверка снимком).
#[test]
fn token_table_snapshot() {
    let (t, _) = Tokens::build(
        &probe(AccentPalette::from_base(rgb(0x00_78D4)), false),
        &UiCfg::default(),
    );
    insta::assert_snapshot!(table(&t));
}
