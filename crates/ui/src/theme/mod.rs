//! Темы оформления Урфина (T-01, S0-decisions №59–№61, `docs/THEMES.md`): роли, 6
//! встроенных тем, пользовательские `*.toml` с `extends`, режим «как в ОС» и высокая
//! контрастность. Без gpui: виды получают цвета через [`crate::tokens`].
//!
//! Роли темы — язык дизайна (frame, panel, header, …). Токены S2, которыми рисуют
//! виды, выводятся из них в [`crate::tokens::Tokens::from_theme`] (таблица там же).
//!
//! Контраст: текстовые роли и синтаксис к `panel` должны давать ≥ 4,5:1. Если цвет
//! темы не дотягивает, он затемняется (светлая тема) или осветляется (тёмная) по
//! тону до порога — сам файл темы не меняется, в лог уходит предупреждение. Так у
//! утверждённой сепии `secondary`/`exec`/комментарий (4,3:1) — на 1–2 % темнее.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;
use toml::Spanned;

use crate::accent::AccentPalette;
use crate::color::Color32;
use crate::sys::HcColors;
use crate::tokens::{MIN_CONTRAST, contrast};

macro_rules! roles {
    ($($f:ident),+ $(,)?) => {
        /// Роли темы (`docs/THEMES.md`) и статусные error/warning/success.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct Roles { $(pub $f: Color32,)+ }

        impl Roles {
            /// Имена ролей, как в TOML.
            pub const NAMES: &[&str] = &[$(stringify!($f),)+];

            fn slot(&mut self, name: &str) -> Option<&mut Color32> {
                match name { $(stringify!($f) => Some(&mut self.$f),)+ _ => None }
            }

            pub fn get(&self, name: &str) -> Option<Color32> {
                match name { $(stringify!($f) => Some(self.$f),)+ _ => None }
            }
        }
    };
}

roles!(
    frame,
    panel,
    header,
    text,
    secondary,
    accent,
    accent_text,
    accent_soft,
    cursor,
    cursor_text,
    border,
    current_line,
    highlight,
    modified,
    exec,
    archive,
    context_admin,
    context_remote,
    error,
    warning,
    success,
);

/// Цвета подсветки синтаксиса (под tree-sitter, №53).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Syntax {
    pub keyword: Color32,
    pub string: Color32,
    pub number: Color32,
    pub comment: Color32,
}

impl Syntax {
    pub const NAMES: &[&str] = &["keyword", "string", "number", "comment"];

    fn slot(&mut self, name: &str) -> Option<&mut Color32> {
        Some(match name {
            "keyword" => &mut self.keyword,
            "string" => &mut self.string,
            "number" => &mut self.number,
            "comment" => &mut self.comment,
            _ => return None,
        })
    }
}

/// Тема: id, роли, синтаксис.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub id: String,
    pub roles: Roles,
    pub syntax: Syntax,
    /// Тёмная: `panel` темнее середины (иконки, системные контролы, gpui-kit).
    pub dark: bool,
}

/// Встроенные темы: id и TOML.
pub const BUILTIN: [(&str, &str); 6] = [
    ("system-light", include_str!("builtin/system-light.toml")),
    ("system-dark", include_str!("builtin/system-dark.toml")),
    ("blue", include_str!("builtin/blue.toml")),
    ("turquoise", include_str!("builtin/turquoise.toml")),
    ("grass", include_str!("builtin/grass.toml")),
    ("sepia", include_str!("builtin/sepia.toml")),
];

/// Выбор темы «как в ОС» (`[ui] theme`, `ui.theme.follow_os`).
pub const AUTO: &str = "auto";

/// Ошибка файла темы: где и что.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeError {
    pub file: String,
    /// С 1; 0 — без места (файл не прочитан, неизвестная база).
    pub line: usize,
    pub msg: String,
}

impl fmt::Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line > 0 {
            write!(f, "{}:{}: {}", self.file, self.line, self.msg)
        } else {
            write!(f, "{}: {}", self.file, self.msg)
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    id: Option<Spanned<String>>,
    extends: Option<Spanned<String>>,
    #[serde(default)]
    roles: toml::Table,
    #[serde(default)]
    syntax: toml::Table,
}

/// Разобрать TOML темы. `base` — тема для `extends` по id (встроенные и уже
/// загруженные). Без `extends` нужны все роли темы (error/warning/success —
/// необязательны: по умолчанию `modified`/`context_remote`/`exec`; `accent_text`,
/// `accent_soft` — выводятся из `accent`, см. [`derive_accent`]) и весь синтаксис.
pub fn parse(
    text: &str,
    file: &str,
    fallback_id: &str,
    base: &dyn Fn(&str) -> Option<Theme>,
) -> Result<Theme, ThemeError> {
    let line_of = |pos: usize| text[..pos.min(text.len())].matches('\n').count() + 1;
    let err = |line: usize, msg: String| ThemeError {
        file: file.to_owned(),
        line,
        msg,
    };
    let raw: Raw = toml::from_str(text).map_err(|e| {
        let line = e.span().map_or(0, |s| line_of(s.start));
        err(line, e.message().to_owned())
    })?;
    let id = raw
        .id
        .as_ref()
        .map_or_else(|| fallback_id.to_owned(), |s| s.get_ref().clone());
    let (mut theme, partial) = match &raw.extends {
        Some(b) => {
            let Some(t) = base(b.get_ref()) else {
                return Err(err(
                    line_of(b.span().start),
                    format!("unknown base theme {:?}", b.get_ref()),
                ));
            };
            (t, true)
        }
        None => (blank(), false),
    };
    theme.id = id;
    let mut seen_roles = Vec::new();
    for (name, value) in &raw.roles {
        let line = locate(text, "roles", name).map_or(0, line_of);
        let c = color(value).ok_or_else(|| err(line, format!("{name}: expected \"#RRGGBB\"")))?;
        let slot = theme
            .roles
            .slot(name)
            .ok_or_else(|| err(line, format!("unknown role {name:?}")))?;
        *slot = c;
        seen_roles.push(name.as_str());
    }
    let mut seen_syntax = Vec::new();
    for (name, value) in &raw.syntax {
        let line = locate(text, "syntax", name).map_or(0, line_of);
        let c = color(value).ok_or_else(|| err(line, format!("{name}: expected \"#RRGGBB\"")))?;
        let slot = theme
            .syntax
            .slot(name)
            .ok_or_else(|| err(line, format!("unknown syntax color {name:?}")))?;
        *slot = c;
        seen_syntax.push(name.as_str());
    }
    // Палитра B: не заданные явно `accent_text`/`accent_soft` — из `accent` (и при
    // `extends`, если файл меняет их исходные роли).
    let inputs = ["accent", "panel", "frame", "header"];
    if !partial || seen_roles.iter().any(|n| inputs.contains(n)) {
        let dark = is_dark(theme.roles.panel);
        let (text, soft) = derive_accent(&theme.roles, dark);
        if !seen_roles.contains(&"accent_text") {
            theme.roles.accent_text = text;
        }
        if !seen_roles.contains(&"accent_soft") {
            theme.roles.accent_soft = soft;
        }
    }
    if !partial {
        let optional = ["error", "warning", "success", "accent_text", "accent_soft"];
        if let Some(missing) = Roles::NAMES
            .iter()
            .find(|n| !optional.contains(n) && !seen_roles.contains(n))
        {
            return Err(err(0, format!("role {missing:?} is missing")));
        }
        if let Some(missing) = Syntax::NAMES.iter().find(|n| !seen_syntax.contains(n)) {
            return Err(err(0, format!("syntax color {missing:?} is missing")));
        }
        let r = &mut theme.roles;
        if !seen_roles.contains(&"error") {
            r.error = r.modified;
        }
        if !seen_roles.contains(&"warning") {
            r.warning = r.context_remote;
        }
        if !seen_roles.contains(&"success") {
            r.success = r.exec;
        }
    }
    theme.dark = is_dark(theme.roles.panel);
    Ok(theme)
}

/// Смещение строки `name = …` внутри таблицы `[table]` — для номера строки ошибки.
fn locate(text: &str, table: &str, name: &str) -> Option<usize> {
    let start = text.find(&format!("[{table}]"))?;
    let mut pos = start;
    for (i, line) in text[start..].split_inclusive('\n').enumerate() {
        let at = pos;
        pos += line.len();
        if i == 0 {
            continue;
        }
        let t = line.trim_start();
        if t.starts_with('[') {
            return None;
        }
        if t.strip_prefix(name)
            .is_some_and(|rest| rest.trim_start().starts_with('='))
        {
            return Some(at);
        }
    }
    None
}

fn blank() -> Theme {
    let z = Color32::BLACK;
    Theme {
        id: String::new(),
        roles: Roles {
            frame: z,
            panel: z,
            header: z,
            text: z,
            secondary: z,
            accent: z,
            accent_text: z,
            accent_soft: z,
            cursor: z,
            cursor_text: z,
            border: z,
            current_line: z,
            highlight: z,
            modified: z,
            exec: z,
            archive: z,
            context_admin: z,
            context_remote: z,
            error: z,
            warning: z,
            success: z,
        },
        syntax: Syntax {
            keyword: z,
            string: z,
            number: z,
            comment: z,
        },
        dark: false,
    }
}

fn color(value: &toml::Value) -> Option<Color32> {
    let hex = value.as_str()?.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let [_, red, green, blue] = u32::from_str_radix(hex, 16).ok()?.to_be_bytes();
    Some(Color32::from_rgb(red, green, blue))
}

/// Роли палитры B из `accent` (решение 2026-11-01): `accent_text` — `accent`,
/// затемнённый (в тёмной — осветлённый) до 4,5:1 к `frame` и `header`; `accent_soft` —
/// 8 % `accent` на `panel`, в тёмной — 16 %.
pub fn derive_accent(r: &Roles, dark: bool) -> (Color32, Color32) {
    let text = toward_contrast(toward_contrast(r.accent, r.frame, dark), r.header, dark);
    let soft = mix(r.panel, r.accent, if dark { 0.16 } else { 0.08 });
    (text, soft)
}

fn is_dark(c: Color32) -> bool {
    contrast(c, Color32::BLACK) < contrast(c, Color32::WHITE)
}

/// Встроенные темы (разбор один раз; встроенные корректны — тест).
pub fn builtin() -> &'static [Theme] {
    static B: OnceLock<Vec<Theme>> = OnceLock::new();
    B.get_or_init(|| {
        BUILTIN
            .iter()
            .filter_map(|(id, text)| {
                parse(text, id, id, &|_| None)
                    .map_err(|e| tracing::error!(error = %e, "builtin theme"))
                    .ok()
            })
            .collect()
    })
}

pub fn builtin_by_id(id: &str) -> Option<&'static Theme> {
    builtin().iter().find(|t| t.id == id)
}

/// Пользовательские темы из каталога (`themes/*.toml`, по имени файла). Вызывать не в
/// UI-потоке. `extends` — встроенная или уже загруженная (по алфавиту) тема.
pub fn load_dir(dir: &Path) -> (Vec<Theme>, Vec<ThemeError>) {
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("toml"))
            })
            .collect(),
        Err(_) => return (Vec::new(), Vec::new()),
    };
    files.sort();
    let mut themes: Vec<Theme> = Vec::new();
    let mut errors = Vec::new();
    for path in files {
        let file = path.display().to_string();
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                errors.push(ThemeError {
                    file,
                    line: 0,
                    msg: e.to_string(),
                });
                continue;
            }
        };
        let base = |id: &str| {
            themes
                .iter()
                .find(|t| t.id == id)
                .cloned()
                .or_else(|| builtin_by_id(id).cloned())
        };
        match parse(&text, &file, &stem, &base) {
            Ok(t) => {
                themes.retain(|x| x.id != t.id);
                themes.push(t);
            }
            Err(e) => errors.push(e),
        }
    }
    (themes, errors)
}

/// Текстовые роли (кроме знаков контекста) — к `panel` не ниже [`MIN_CONTRAST`];
/// `cursor_text` — к `cursor`.
/// Возвращает подтянутые (имя, было, стало).
pub fn ensure_text_contrast(t: &mut Theme) -> Vec<(&'static str, Color32, Color32)> {
    let mut changed = Vec::new();
    let panel = t.roles.panel;
    let dark = t.dark;
    let mut fix = |name: &'static str, c: &mut Color32, bg: Color32| {
        let fixed = toward_contrast(*c, bg, dark);
        if fixed != *c {
            changed.push((name, *c, fixed));
            *c = fixed;
        }
    };
    let r = &mut t.roles;
    fix("text", &mut r.text, panel);
    fix("secondary", &mut r.secondary, panel);
    fix("modified", &mut r.modified, panel);
    fix("exec", &mut r.exec, panel);
    fix("archive", &mut r.archive, panel);
    // context_admin/context_remote — полоса и знак контекста, не текст списка: как
    // утверждено (у янтаря ≈ 4:1).
    fix("error", &mut r.error, panel);
    fix("warning", &mut r.warning, panel);
    fix("success", &mut r.success, panel);
    let cursor = r.cursor;
    fix("cursor_text", &mut r.cursor_text, cursor);
    // Клавиши keybar и части чипов — на `frame`/`header`.
    let (frame, header) = (r.frame, r.header);
    fix("accent_text", &mut r.accent_text, frame);
    fix("accent_text", &mut r.accent_text, header);
    let s = &mut t.syntax;
    fix("keyword", &mut s.keyword, panel);
    fix("string", &mut s.string, panel);
    fix("number", &mut s.number, panel);
    fix("comment", &mut s.comment, panel);
    changed
}

/// Сдвиг к чёрному (светлый фон) или белому (тёмный) малыми шагами до порога.
pub fn toward_contrast(fg: Color32, bg: Color32, dark: bool) -> Color32 {
    let target = if dark { Color32::WHITE } else { Color32::BLACK };
    let mut c = fg;
    for step in 1..=100u8 {
        if contrast(c, bg) >= MIN_CONTRAST {
            break;
        }
        c = mix(fg, target, f32::from(step) / 100.0);
    }
    c
}

/// Смесь `a` и `b`: доля `t` от `b`.
#[allow(clippy::cast_sign_loss, reason = "значения 0..=255")]
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// «Как в ОС»: системная светлая или тёмная с акцентом Windows. Курсор — смесь
/// акцента с `panel` (≈18 % светлая, ≈35 % тёмная), текст на нём — с контрастом.
pub fn system(dark: bool, accent: Option<&AccentPalette>) -> Theme {
    let id = if dark { "system-dark" } else { "system-light" };
    let mut t = builtin_by_id(id).cloned().unwrap_or_else(blank);
    if let Some(a) = accent {
        // Как WinUI: на светлом — тёмный вариант акцента, на тёмном — светлый.
        let acc = if dark { a.light2 } else { a.dark1 };
        t.roles.accent = acc;
        t.roles.cursor = mix(t.roles.panel, acc, if dark { 0.35 } else { 0.18 });
        (t.roles.accent_text, t.roles.accent_soft) = derive_accent(&t.roles, dark);
    }
    t
}

/// Высокая контрастность Windows (`GetSysColor`): перекрывает выбор темы.
pub fn high_contrast(hc: &HcColors) -> Theme {
    let mut t = blank();
    t.id = "high-contrast".into();
    let r = &mut t.roles;
    for c in [
        &mut r.frame,
        &mut r.panel,
        &mut r.header,
        &mut r.current_line,
        // Палитра B в высокой контрастности не действует: тона нет.
        &mut r.accent_soft,
    ] {
        *c = hc.window;
    }
    for c in [
        &mut r.text,
        &mut r.accent_text,
        &mut r.border,
        &mut r.modified,
        &mut r.exec,
        &mut r.archive,
        &mut r.context_admin,
        &mut r.context_remote,
        &mut r.error,
        &mut r.warning,
        &mut r.success,
    ] {
        *c = hc.window_text;
    }
    r.secondary = hc.gray_text;
    r.accent = hc.highlight;
    r.cursor = hc.highlight;
    r.highlight = hc.highlight;
    r.cursor_text = hc.highlight_text;
    let s = &mut t.syntax;
    for c in [&mut s.keyword, &mut s.string, &mut s.number, &mut s.comment] {
        *c = hc.window_text;
    }
    t.dark = is_dark(hc.window);
    t
}

/// Выбор темы и то, откуда её брать.
#[derive(Clone, Debug, Default)]
pub struct Themes {
    /// Пользовательские (`themes/*.toml`), загружены не в UI-потоке.
    pub user: Vec<Theme>,
}

impl Themes {
    /// Id всех тем для `ui.theme.next` и настроек: встроенные, затем свои.
    pub fn ids(&self) -> Vec<String> {
        builtin()
            .iter()
            .map(|t| t.id.clone())
            .chain(self.user.iter().map(|t| t.id.clone()))
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<Theme> {
        self.user
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .or_else(|| builtin_by_id(id).cloned())
    }

    /// Тема к показу: высокая контрастность ОС → она; `auto` или неизвестная → «как в
    /// ОС». Текстовые роли подтянуты по контрасту.
    pub fn resolve(&self, choice: &str, os: &OsLook) -> Theme {
        if let Some(hc) = os.high_contrast {
            return high_contrast(&hc);
        }
        let mut t = match choice {
            AUTO | "" => system(os.dark, os.accent.as_ref()),
            id => self
                .get(id)
                .unwrap_or_else(|| system(os.dark, os.accent.as_ref())),
        };
        let own = builtin_by_id(&t.id).is_none();
        for (role, was, now) in ensure_text_contrast(&mut t) {
            // Встроенные подтянуты по решению (сепия, 1–2 %); своя тема — повод исправить файл.
            if own {
                tracing::warn!(theme = %t.id, role, ?was, ?now, "theme: contrast below 4.5:1, raised");
            } else {
                tracing::debug!(theme = %t.id, role, ?was, ?now, "theme: contrast raised");
            }
        }
        t
    }
}

/// Что от ОС нужно теме.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OsLook {
    pub dark: bool,
    pub accent: Option<AccentPalette>,
    pub high_contrast: Option<HcColors>,
}

/// Старые значения `[ui] theme` (S2): `system` → `auto`, `light`/`dark` → системные.
pub fn normalize_choice(s: &str) -> String {
    match s.trim().to_ascii_lowercase().as_str() {
        "" | "system" | "auto" => AUTO.into(),
        "light" => "system-light".into(),
        "dark" => "system-dark".into(),
        other => other.to_owned(),
    }
}

#[cfg(test)]
mod tests;
