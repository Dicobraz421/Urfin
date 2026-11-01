//! Темы (T-01-1): встроенные, контраст, разбор пользовательских, «как в ОС».

use std::time::{Duration, Instant};

use super::*;

#[test]
fn six_builtin_themes_in_order() {
    let ids: Vec<&str> = builtin().iter().map(|t| t.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "system-light",
            "system-dark",
            "blue",
            "turquoise",
            "grass",
            "sepia"
        ]
    );
    assert!(builtin_by_id("system-dark").unwrap().dark);
    assert!(!builtin_by_id("sepia").unwrap().dark);
}

/// Разбор всех встроенных при старте — ≤ 1 мс суммарно в release (лучший из 20;
/// отладочная сборка без оптимизаций и под нагрузкой соседних тестов — до 5 мс).
#[test]
fn builtin_parse_under_1ms() {
    let budget = Duration::from_millis(if cfg!(debug_assertions) { 5 } else { 1 });
    let best = (0..20)
        .map(|_| {
            let t0 = Instant::now();
            for (id, text) in BUILTIN {
                parse(text, id, id, &|_| None).unwrap();
            }
            t0.elapsed()
        })
        .min()
        .unwrap();
    assert!(best < budget, "{best:?}");
}

#[test]
fn builtin_contrast_and_rules() {
    for t in builtin() {
        assert_ne!(t.roles.panel, Color32::WHITE, "{}: no pure white", t.id);
        let mut fixed = t.clone();
        let changed = ensure_text_contrast(&mut fixed);
        // Утверждённые палитры: подтягивается только сепия и только на 1–2 %
        // (error/warning/success — производные, не из THEMES.md).
        for (role, was, now) in changed
            .iter()
            .filter(|(r, _, _)| !["error", "warning", "success"].contains(r))
        {
            assert_eq!(t.id, "sepia", "{}: {role} below 4.5:1", t.id);
            let d = |a: u8, b: u8| a.abs_diff(b);
            assert!(
                d(was.r(), now.r()) <= 6 && d(was.g(), now.g()) <= 6 && d(was.b(), now.b()) <= 6,
                "{role}: {was:?} → {now:?}"
            );
        }
        let r = &fixed.roles;
        for (name, c) in [
            ("text", r.text),
            ("secondary", r.secondary),
            ("modified", r.modified),
            ("exec", r.exec),
            ("archive", r.archive),
            ("keyword", fixed.syntax.keyword),
            ("string", fixed.syntax.string),
            ("number", fixed.syntax.number),
            ("comment", fixed.syntax.comment),
        ] {
            let k = contrast(c, r.panel);
            assert!(k >= MIN_CONTRAST, "{} {name}: {k:.2}", t.id);
        }
        assert!(
            contrast(r.cursor_text, r.cursor) >= MIN_CONTRAST,
            "{}",
            t.id
        );
        let de = delta_e(t.roles.exec, t.roles.accent);
        assert!(de > 20.0, "{}: exec vs accent ΔE {de:.1}", t.id);
        // Палитра B (решение 2026-11-01, THEMES.md): пары ролей ≥ 4,5:1.
        let mut pairs = vec![
            ("accent_text/frame", r.accent_text, r.frame),
            ("accent_text/header", r.accent_text, r.header),
            ("text/accent_soft", r.text, r.accent_soft),
            ("secondary/accent_soft", r.secondary, r.accent_soft),
            ("modified/current_line", r.modified, r.current_line),
            ("exec/current_line", r.exec, r.current_line),
            ("archive/current_line", r.archive, r.current_line),
            ("exec/cursor", r.exec, r.cursor),
        ];
        // В тёмной `modified`/`archive` к `cursor` — 3,5 и 4,3, как до палитры B.
        if !t.dark {
            pairs.push(("modified/cursor", r.modified, r.cursor));
            pairs.push(("archive/cursor", r.archive, r.cursor));
        }
        for (what, fg, bg) in pairs {
            let k = contrast(fg, bg);
            assert!(k >= MIN_CONTRAST, "{} {what}: {k:.2}", t.id);
        }
        assert_eq!(
            r.context_admin, r.modified,
            "{}: context_admin = modified",
            t.id
        );
    }
}

/// Без `accent_text`/`accent_soft` в файле — выводятся из `accent` (палитра B).
#[test]
fn accent_roles_derive_when_absent() {
    let light = builtin_by_id("system-light").unwrap();
    let text = BUILTIN[0]
        .1
        .lines()
        .filter(|l| !l.starts_with("accent_text") && !l.starts_with("accent_soft"))
        .collect::<Vec<_>>()
        .join("\n");
    let t = parse(&text, "x.toml", "x", &|_| None).unwrap();
    let r = &t.roles;
    assert!(contrast(r.accent_text, r.frame) >= MIN_CONTRAST);
    assert!(contrast(r.accent_text, r.header) >= MIN_CONTRAST);
    assert_eq!(r.accent_soft, mix(r.panel, r.accent, 0.08));
    assert!(
        light.roles.accent_soft.r().abs_diff(r.accent_soft.r()) <= 2,
        "8 % accent на panel ≈ THEMES.md"
    );
    // `extends` со своим акцентом — роли пересчитаны.
    let own = parse(
        "extends = \"system-light\"\n[roles]\naccent = \"#8C6035\"\n",
        "y.toml",
        "y",
        &base,
    )
    .unwrap();
    assert_ne!(own.roles.accent_soft, light.roles.accent_soft);
    assert!(contrast(own.roles.accent_text, own.roles.frame) >= MIN_CONTRAST);
}

/// CIE76 ΔE в Lab (D65).
#[allow(clippy::many_single_char_names, reason = "формулы XYZ и Lab")]
fn delta_e(a: Color32, b: Color32) -> f32 {
    fn lab(c: Color32) -> [f32; 3] {
        let lin = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let (r, g, b) = (lin(c.r()), lin(c.g()), lin(c.b()));
        let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.950_47;
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.088_83;
        let f = |t: f32| {
            if t > 0.008_856 {
                t.cbrt()
            } else {
                7.787 * t + 16.0 / 116.0
            }
        };
        let (fx, fy, fz) = (f(x), f(y), f(z));
        [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
    }
    let (p, q) = (lab(a), lab(b));
    ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt()
}

fn base(id: &str) -> Option<Theme> {
    builtin_by_id(id).cloned()
}

#[test]
fn user_theme_extends_and_errors_with_lines() {
    let t = parse(
        "extends = \"system-light\"\n[roles]\naccent = \"#AA0000\"\n",
        "red.toml",
        "red",
        &base,
    )
    .unwrap();
    assert_eq!(t.id, "red");
    assert_eq!(t.roles.accent, Color32::from_rgb(0xAA, 0, 0));
    assert_eq!(
        t.roles.panel,
        builtin_by_id("system-light").unwrap().roles.panel
    );

    let e = parse(
        "extends = \"system-light\"\n[roles]\npanel = \"#F4F4F4\"\nglow = \"#000000\"\n",
        "x.toml",
        "x",
        &base,
    )
    .unwrap_err();
    assert_eq!((e.line, e.msg.as_str()), (4, "unknown role \"glow\""));
    assert_eq!(e.to_string(), "x.toml:4: unknown role \"glow\"");

    let e = parse(
        "extends = \"system-dark\"\n\n[roles]\ntext = \"red\"\n",
        "x.toml",
        "x",
        &base,
    )
    .unwrap_err();
    assert_eq!(e.line, 4, "{e}");

    let e = parse("extends = \"nope\"\n", "x.toml", "x", &base).unwrap_err();
    assert_eq!(e.line, 1);
    let e = parse("[roles]\nframe = \"#000000\"\n", "x.toml", "x", &base).unwrap_err();
    assert!(e.msg.contains("missing"), "{e}");
    let e = parse("[roles\n", "x.toml", "x", &base).unwrap_err();
    assert_eq!(e.line, 1, "{e}");
    let e = parse("color = 1\n", "x.toml", "x", &base).unwrap_err();
    assert!(e.msg.contains("unknown field"), "{e}");
}

#[test]
fn load_dir_resolves_chain_and_reports_errors() {
    let d = tempfile::Builder::new()
        .prefix("urfin-themes-")
        .tempdir()
        .unwrap();
    std::fs::write(
        d.path().join("a-night.toml"),
        "extends = \"system-dark\"\n[roles]\naccent = \"#00AA00\"\n",
    )
    .unwrap();
    std::fs::write(
        d.path().join("b-night2.toml"),
        "id = \"night2\"\nextends = \"a-night\"\n[roles]\npanel = \"#202020\"\n",
    )
    .unwrap();
    std::fs::write(d.path().join("c-bad.toml"), "[roles]\npanel = 5\n").unwrap();
    let (themes, errors) = load_dir(d.path());
    let ids: Vec<&str> = themes.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["a-night", "night2"]);
    assert_eq!(themes[1].roles.accent, Color32::from_rgb(0, 0xAA, 0));
    assert_eq!(errors.len(), 1);
    assert!(errors[0].file.ends_with("c-bad.toml"));
    assert_eq!(errors[0].line, 2);
    assert_eq!(load_dir(&d.path().join("none")).0.len(), 0);
}

#[test]
fn os_mode_takes_accent_and_mixes_cursor() {
    let accent = AccentPalette::from_base(Color32::from_rgb(0xC3, 0x00, 0x52));
    let t = system(false, Some(&accent));
    assert_eq!(t.id, "system-light");
    assert_eq!(t.roles.accent, accent.dark1);
    assert_eq!(t.roles.cursor, mix(t.roles.panel, accent.dark1, 0.18));
    let os = OsLook {
        dark: true,
        accent: Some(accent),
        high_contrast: None,
    };
    let themes = Themes::default();
    let auto = themes.resolve(AUTO, &os);
    assert_eq!(auto.id, "system-dark");
    assert!(contrast(auto.roles.cursor_text, auto.roles.cursor) >= MIN_CONTRAST);
    // Своя тема акцент ОС не берёт; неизвестная — «как в ОС».
    assert_eq!(
        themes.resolve("grass", &os).roles.accent,
        builtin_by_id("grass").unwrap().roles.accent
    );
    assert_eq!(themes.resolve("nope", &os).id, "system-dark");
}

#[test]
fn high_contrast_overrides_choice() {
    let hc = HcColors {
        window: Color32::BLACK,
        window_text: Color32::WHITE,
        highlight: Color32::from_rgb(0x1A, 0xEB, 0xFF),
        highlight_text: Color32::BLACK,
        gray_text: Color32::from_rgb(0x3F, 0xF2, 0x3F),
        hotlight: Color32::from_rgb(0xFF, 0xFF, 0x00),
    };
    let os = OsLook {
        dark: false,
        accent: None,
        high_contrast: Some(hc),
    };
    let t = Themes::default().resolve("sepia", &os);
    assert_eq!(t.id, "high-contrast");
    assert!(t.dark);
    assert_eq!(t.roles.panel, Color32::BLACK);
}

#[test]
fn old_config_values() {
    assert_eq!(normalize_choice("system"), AUTO);
    assert_eq!(normalize_choice("Light"), "system-light");
    assert_eq!(normalize_choice("dark"), "system-dark");
    assert_eq!(normalize_choice("grass"), "grass");
}

/// T-01-6: цвета в `ui` — только роли темы. Вне `theme` и тестов нельзя `rgb(0x…)`,
/// `rgba(0x…)`, `hsla(<число>…)`, `Hsla {` и строк `"#RRGGBB"`; исключение — строка с
/// комментарием `// colors: allow <причина>`.
#[test]
fn no_color_literals() {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n != "theme") {
                    walk(&p, out);
                }
            } else if p.extension().is_some_and(|x| x == "rs")
                && !p.to_string_lossy().ends_with("tests.rs")
            {
                out.push(p);
            }
        }
    }
    let hex6 = |s: &str| {
        s.split('"').skip(1).step_by(2).any(|lit| {
            lit.as_bytes()
                .windows(7)
                .any(|w| w[0] == b'#' && w[1..].iter().all(u8::is_ascii_hexdigit))
        })
    };
    let num_arg = |s: &str, pat: &str| {
        s.match_indices(pat).any(|(i, _)| {
            let before = s[..i].chars().last();
            let word_start = !before.is_some_and(|c| c.is_alphanumeric() || c == '_');
            let rest = s[i + pat.len()..].trim_start();
            word_start && rest.starts_with(|c: char| c.is_ascii_digit())
        })
    };
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&root, &mut files);
    let mut bad = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("#[cfg(test)]") {
                break; // тестовый код ниже
            }
            if line.contains("// colors: allow") {
                continue;
            }
            let code = line.split("//").next().unwrap_or_default();
            if num_arg(code, "rgb(")
                || num_arg(code, "rgba(")
                || num_arg(code, "hsla(")
                || code.replace("-> Hsla {", "").contains("Hsla {")
                || hex6(code)
            {
                bad.push(format!("{}:{}: {}", f.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(bad.is_empty(), "color literals:\n{}", bad.join("\n"));
}
