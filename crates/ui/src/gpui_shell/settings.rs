//! Настройки — вкладка на место панелей (D-08, `screen-settings*.png`, `screen-about.png`):
//! разделы слева 220, справа форма — строки 36: подпись 240, контрол (сегменты, флажок),
//! пояснение `secondary`. Изменения применяются сразу: команды ui, тема, клавиши или
//! `ui.setting` (запись в конфиг и перечитывание). Показаны только настройки, что уже
//! работают; разделы будущих частей (терминал) — с ними. Разделы «SSH» и «Пакеты» —
//! `settings_ssh`, `settings_pkg`.

use gpui_kit::prelude::*;
use gpui_kit::{Div, SharedString, div, px};
use urfin_core::{AppState, Command, PolicyKey, PolicySet, PolicySource, PolicyValue};

use super::overlays::ClickAll;
use super::screens::Scene;
use super::settings_pkg::Fields as PkgFields;
use super::settings_ssh::Fields;
use super::theme::{Theme, hsla};
use crate::UiState;
use crate::tokens::Role;

/// Разделы и ключи подписей; «Подключения» — подраздел «SSH».
pub(super) const SECTIONS: [(&str, &str); 12] = [
    ("view", "ui-set-view"),
    ("panels", "ui-set-panels"),
    ("keys", "ui-set-keys"),
    ("editor", "ui-set-editor"),
    ("viewer", "ui-set-viewer"),
    ("ssh", "net-set-ssh"),
    ("conn", "net-set-conn"),
    ("ops", "ui-set-ops"),
    ("policies", "ui-set-policies"),
    ("pkg", "pkg-set-section"),
    ("a11y", "ui-set-a11y"),
    ("about", "ui-set-about"),
];

const W_NAV: f32 = 220.0;
const W_LABEL: f32 = 240.0;
/// Ширина выпадающего списка (DESIGN.md: «список ≥ 260»).
const W_LIST: f32 = 260.0;
/// Строка 36 и сегменты 28 (DESIGN.md, `screen-settings-panels`).
const H_ROW: f32 = 36.0;
/// Замок (Segoe Fluent Icons `Lock`).
const LOCK: &str = "\u{E72E}";
/// Шеврон списка (Segoe Fluent Icons `ChevronDown`).
const CHEVRON: &str = "\u{E70D}";

/// Иконка приложения (ассет, свои цвета — не роли темы; DESIGN.md «О программе»).
static ICON: &[u8] = include_bytes!("../../../../assets/icon/urfin-256.png");

/// Значение ключа конфига (`ops.defaults.conflict`) текстом; `None` — не задан.
fn cfg_value(s: &AppState, key: &str) -> Option<String> {
    let mut parts = key.split('.');
    let first = parts.next()?;
    let c = &s.config;
    let mut v: toml::Value = match first {
        "panels" => toml::Value::try_from(&c.panels).ok()?,
        "general" => toml::Value::try_from(&c.general).ok()?,
        other => c.ext.get(other)?.clone(),
    };
    for p in parts {
        v = v.get(p)?.clone();
    }
    Some(match v {
        toml::Value::String(s) => s,
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Integer(i) => i.to_string(),
        other => other.to_string(),
    })
}

/// Сегменты: варианты (значение, подпись), текущее, команда по значению.
pub(super) fn segments(
    theme: &Theme,
    id: &str,
    opts: &[(&str, String)],
    current: &str,
    click: ClickAll<'_>,
    cmd: &dyn Fn(&str) -> Vec<Command>,
) -> Div {
    let mut row = div().flex().items_center().gap(px(6.0));
    for (k, (value, label)) in opts.iter().enumerate() {
        let on = value.eq_ignore_ascii_case(current);
        row = row.child(
            div()
                .id(SharedString::from(format!("set-{id}-{k}")))
                .h(px(28.0))
                .px(px(14.0))
                .flex()
                .items_center()
                .rounded(px(4.0))
                .border_1()
                .border_color(theme.c(if on {
                    Role::AccentFill
                } else {
                    Role::StrokeControl
                }))
                .bg(theme.c(if on {
                    Role::FillCursor
                } else {
                    Role::LayerPanel
                }))
                .text_size(px(13.0))
                .child(SharedString::from(label.clone()))
                .on_click(click(cmd(value))),
        );
    }
    row
}

/// Флажок с подписью; клик — команды.
pub(super) fn check(theme: &Theme, id: &str, on: bool, label: String) -> gpui_kit::Stateful<Div> {
    super::strip::check(theme, format!("set-{id}"), on, label, true)
}

/// Строка формы: подпись, контрол, пояснение справа.
pub(super) fn row(theme: &Theme, label: String, control: Div, note: Option<String>) -> Div {
    row_with(theme, div().child(SharedString::from(label)), control, note)
}

/// Как [`row`], подпись — элементом (с замком политики).
fn row_with(theme: &Theme, label: Div, control: Div, note: Option<String>) -> Div {
    div()
        .min_h(px(H_ROW))
        .py(px(4.0))
        .flex()
        .items_center()
        .border_b_1()
        .border_color(theme.c(Role::StrokeDivider))
        .child(label.w(px(W_LABEL)).flex_none().flex().items_center())
        .child(div().flex_1().min_w_0().child(control))
        .children(note.map(|n| {
            div()
                .flex_none()
                .text_size(px(12.0))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(n))
        }))
}

/// Задан ли ключ конфига (`ops.delete_to_trash`) политикой организации (ADR-0020).
fn locked(s: &AppState, key: &str) -> bool {
    let path: Vec<&str> = key.split('.').collect();
    s.ext
        .get::<PolicySet>()
        .is_some_and(|p| p.setting(&path).is_some())
}

/// Замок политики после подписи, `secondary`.
fn lock(theme: &Theme) -> Div {
    div()
        .ml(px(6.0))
        .font_family("Segoe Fluent Icons")
        .text_size(px(11.0))
        .text_color(theme.c(Role::TextSecondary))
        .child(SharedString::new_static(LOCK))
}

/// Строка с ключом, который может задать политика (`screen-settings-ops.png`): тогда
/// замок у подписи, контрол недоступен (`control(false)` — без кликов) и пояснение
/// «Задано политикой организации».
fn policy_row(
    s: &AppState,
    theme: &Theme,
    key: &str,
    label: String,
    control: &dyn Fn(bool) -> Div,
    note: Option<String>,
) -> Div {
    if !locked(s, key) {
        return row(theme, label, control(true), note);
    }
    locked_row(
        theme,
        label,
        control,
        urfin_i18n::current().get("ui-set-by-policy"),
    )
}

/// Как [`policy_row`], пояснение — с источником (`screen-settings-ssh.png`): «Задано
/// политикой организации · GPO».
fn sourced_row(
    s: &AppState,
    theme: &Theme,
    key: &str,
    label: String,
    control: &dyn Fn(bool) -> Div,
    note: Option<String>,
) -> Div {
    let path: Vec<&str> = key.split('.').collect();
    match s.ext.get::<PolicySet>().and_then(|p| p.setting(&path)) {
        None => row(theme, label, control(true), note),
        Some(source) => locked_row(
            theme,
            label,
            control,
            urfin_i18n::current().fmt("net-remember-locked", &arg1("source", source.to_string())),
        ),
    }
}

fn locked_row(theme: &Theme, label: String, control: &dyn Fn(bool) -> Div, note: String) -> Div {
    row_with(
        theme,
        div().child(SharedString::from(label)).child(lock(theme)),
        div().opacity(0.6).child(control(false)),
        Some(note),
    )
}

/// Заголовок группы 15/500.
pub(super) fn group(text: String) -> Div {
    div()
        .pt(px(18.0))
        .pb(px(6.0))
        .text_size(px(15.0))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .child(SharedString::from(text))
}

pub(super) fn set(key: &str, value: &str) -> Vec<Command> {
    vec![
        Command::new("ui.setting")
            .arg("key", key)
            .arg("value", value),
    ]
}

/// Вся вкладка настроек: навигация и раздел.
pub(super) fn view(
    s: &AppState,
    section: &str,
    theme: &Theme,
    click: ClickAll<'_>,
    fields: (&Fields, &PkgFields),
    keys: Option<Div>,
    cx: &gpui_kit::Context<super::CoreHost>,
) -> Div {
    let i18n = urfin_i18n::current();
    let mut nav = div()
        .w(px(W_NAV))
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .p(px(8.0))
        .bg(theme.c(Role::BgBase))
        .border_r_1()
        .border_color(theme.c(Role::StrokeDivider));
    for (id, label) in SECTIONS {
        let on = id == section;
        nav = nav.child(
            div()
                .id(SharedString::from(format!("set-nav-{id}")))
                .h(px(32.0))
                .px(px(12.0))
                .when(id == "conn", |d| d.pl(px(28.0)))
                .flex()
                .items_center()
                .rounded(px(4.0))
                .when(on, |d| d.bg(theme.c(Role::FillCursor)))
                .when(!on, |d| d.hover(|d| d.bg(theme.c(Role::FillSubtleHover))))
                .child(SharedString::from(i18n.get(label)))
                .on_click(click(vec![Command::new("ui.settings").arg("section", id)])),
        );
    }
    let body = match section {
        "panels" => panels(s, theme, click),
        "keys" => keys.unwrap_or_else(div),
        "editor" => editor(s, theme, click),
        "viewer" => viewer(s, theme, click),
        "ops" => ops(s, theme, click),
        "ssh" => super::settings_ssh::section(s, theme, click, fields.0),
        "pkg" => super::settings_pkg::section(s, theme, click, fields.1, cx),
        "conn" => {
            let u = s.ext.get::<UiState>();
            super::ssh::settings_conn(
                s,
                theme,
                click,
                u.map_or("", |u| u.conn_filter.as_str()),
                u.map_or(0, |u| u.conn_sel),
            )
        }
        "policies" => policies(s, theme),
        "a11y" => a11y(s, theme, click),
        "about" => about(s, theme, click),
        _ => look(s, theme, click),
    };
    div()
        .size_full()
        .flex()
        .bg(theme.c(Role::LayerPanel))
        .text_size(px(13.0))
        .child(nav)
        .child(
            div()
                .id("settings-body")
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_y_scroll()
                .px(px(32.0))
                .py(px(16.0))
                .child(body),
        )
}

/// «Вид»: тема (как в Windows, карточки), высота строки, иконки, язык, шрифты.
#[allow(clippy::too_many_lines, reason = "форма раздела сверху вниз")]
fn look(s: &AppState, theme: &Theme, click: ClickAll<'_>) -> Div {
    let i18n = urfin_i18n::current();
    let st = s.ext.get::<UiState>();
    let chosen = st.map_or(crate::theme::AUTO, |u| u.theme.as_str());
    let auto = chosen == crate::theme::AUTO;
    let follow =
        check(theme, "follow-os", auto, i18n.get("ui-set-follow-os")).on_click(click(vec![
            if auto {
                Command::new("ui.theme.set").arg("id", theme.tokens.theme.id.as_str())
            } else {
                Command::new("ui.theme.follow_os")
            },
        ]));
    let mut cards = div().flex().gap(px(12.0));
    for t in crate::theme::builtin() {
        let on = !auto && t.id == chosen;
        let r = &t.roles;
        let line = |w: f32, c| div().h(px(3.0)).w(px(w)).rounded(px(1.0)).bg(hsla(c));
        let pane = |accent: bool| {
            div()
                .flex_1()
                .h_full()
                .p(px(6.0))
                .flex()
                .flex_col()
                .gap(px(5.0))
                .bg(hsla(r.panel))
                .child(line(48.0, if accent { r.accent } else { r.secondary }))
                .child(line(52.0, r.secondary))
                .child(line(52.0, if accent { r.cursor } else { r.secondary }))
                .child(line(40.0, r.secondary))
        };
        cards = cards.child(
            div()
                .id(SharedString::from(format!("set-theme-{}", t.id)))
                .flex_1()
                .min_w(px(96.0))
                .p(px(8.0))
                .flex()
                .flex_col()
                .gap(px(8.0))
                .rounded(px(6.0))
                .border_2()
                .border_color(theme.c(if on {
                    Role::AccentFill
                } else {
                    Role::StrokeControl
                }))
                .child(
                    div()
                        .h(px(76.0))
                        .p(px(4.0))
                        .flex()
                        .gap(px(3.0))
                        .rounded(px(3.0))
                        .bg(hsla(r.frame))
                        .child(pane(true))
                        .child(pane(false)),
                )
                .child(
                    div()
                        .text_size(px(12.5))
                        .child(SharedString::from(i18n.get(&format!("ui-theme-{}", t.id)))),
                )
                .on_click(click(vec![
                    Command::new("ui.theme.set").arg("id", t.id.as_str()),
                ])),
        );
    }
    let density = st.map_or(24, |u| u.density).to_string();
    let icons = st.map_or(crate::config::IconMode::default().as_str(), |u| {
        u.icons.as_str()
    });
    let lang = i18n.lang().to_string();
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(group(i18n.get("ui-set-theme")))
                .child(follow),
        )
        .child(cards)
        .child(div().h(px(12.0)))
        .child(row(
            theme,
            i18n.get("ui-set-density"),
            segments(
                theme,
                "density",
                &[
                    ("20", "20 px".into()),
                    ("24", "24 px".into()),
                    ("28", "28 px".into()),
                ],
                &density,
                click,
                &|v| vec![Command::new("ui.density.set").arg("px", v.parse::<i64>().unwrap_or(24))],
            ),
            None,
        ))
        .child(row(
            theme,
            i18n.get("ui-set-icons"),
            segments(
                theme,
                "icons",
                &[
                    ("none", i18n.get("ui-set-icons-none")),
                    ("mono", i18n.get("ui-set-icons-mono")),
                    ("color", i18n.get("ui-set-icons-color")),
                ],
                icons,
                click,
                &|v| vec![Command::new("ui.icons.set").arg("mode", v)],
            ),
            None,
        ))
        .child(row(
            theme,
            i18n.get("ui-set-lang"),
            segments(
                theme,
                "lang",
                &[("ru", "Русский".into()), ("en", "English".into())],
                if lang.starts_with("ru") { "ru" } else { "en" },
                click,
                &|v| vec![Command::new("ui.lang").arg("lang", v)],
            ),
            None,
        ))
        .child(row(
            theme,
            i18n.get("ui-set-fonts"),
            div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(SharedString::from(i18n.get("ui-set-font-ui")))
                .child(
                    div()
                        .font_family("Cascadia Mono")
                        .text_size(px(12.5))
                        .child(SharedString::from(i18n.get("ui-set-font-mono"))),
                ),
            None,
        ))
}

/// «Панели»: режим по умолчанию, сортировка новых вкладок.
fn panels(s: &AppState, theme: &Theme, click: ClickAll<'_>) -> Div {
    let i18n = urfin_i18n::current();
    let mode = cfg_value(s, "panels.view_mode").unwrap_or_else(|| "full".into());
    let key = cfg_value(s, "panels.sort.key").unwrap_or_else(|| "name".into());
    let desc = cfg_value(s, "panels.sort.desc").as_deref() == Some("true");
    let dirs_first = cfg_value(s, "panels.sort.dirs_first").as_deref() != Some("false");
    div()
        .flex()
        .flex_col()
        .child(group(i18n.get("ui-set-panel-view")))
        .child(row(
            theme,
            i18n.get("ui-set-view-mode"),
            segments(
                theme,
                "mode",
                &[
                    ("full", "Full".into()),
                    ("brief", "Brief".into()),
                    ("medium", "Medium".into()),
                    ("wide", "Wide".into()),
                ],
                &mode,
                click,
                &|v| set("panels.view_mode", v),
            ),
            Some(i18n.get("ui-set-for-new-tabs")),
        ))
        .child(row(
            theme,
            i18n.get("ui-set-sort"),
            segments(
                theme,
                "sort",
                &[
                    ("name", i18n.get("ui-sort-name")),
                    ("ext", i18n.get("ui-sort-ext")),
                    ("size", i18n.get("ui-sort-size")),
                    ("mtime", i18n.get("ui-sort-mtime")),
                ],
                &key,
                click,
                &|v| set("panels.sort.key", v),
            ),
            None,
        ))
        .child(row(
            theme,
            String::new(),
            div()
                .flex()
                .gap(px(24.0))
                .child(
                    check(theme, "sort-desc", desc, i18n.get("ui-sort-desc")).on_click(click(set(
                        "panels.sort.desc",
                        if desc { "false" } else { "true" },
                    ))),
                )
                .child(
                    check(
                        theme,
                        "dirs-first",
                        dirs_first,
                        i18n.get("ui-set-dirs-first"),
                    )
                    .on_click(click(set(
                        "panels.sort.dirs_first",
                        if dirs_first { "false" } else { "true" },
                    ))),
                ),
            None,
        ))
}

/// Форма раздела с ключами конфига: каждая строка — через [`policy_row`] (ключ под
/// политикой организации — замок и недоступный контрол, ADR-0020).
#[derive(Clone, Copy)]
pub(super) struct Form<'a> {
    pub s: &'a AppState,
    pub theme: &'a Theme,
    pub click: ClickAll<'a>,
}

impl Form<'_> {
    /// Сегменты ключа `key`: варианты (значение, подпись), текущее.
    pub(super) fn seg(
        &self,
        key: &str,
        label: String,
        opts: &[(&str, String)],
        current: &str,
        note: Option<String>,
    ) -> Div {
        let Form { s, theme, click } = *self;
        let id = key.replace('.', "-");
        sourced_row(
            s,
            theme,
            key,
            label,
            &|on| {
                segments(theme, &id, opts, current, click, &|v| {
                    if on { set(key, v) } else { Vec::new() }
                })
            },
            note,
        )
    }

    /// Флажок ключа `key` с текстом; клик пишет обратное значение.
    pub(super) fn flag(
        &self,
        key: &str,
        label: String,
        value: bool,
        text: &str,
        note: Option<String>,
    ) -> Div {
        let Form { s, theme, click } = *self;
        let id = key.replace('.', "-");
        sourced_row(
            s,
            theme,
            key,
            label,
            &|on| {
                div().child(check(theme, &id, value, text.to_owned()).when(on, |c| {
                    c.on_click(click(set(key, if value { "false" } else { "true" })))
                }))
            },
            note,
        )
    }

    /// Выпадающий список ключа `key` (`select`): клик — следующий вариант, `cmd` —
    /// команды по значению.
    pub(super) fn list(
        &self,
        key: &str,
        label: String,
        opts: &[(&str, String)],
        current: &str,
        cmd: &dyn Fn(&str) -> Vec<Command>,
        note: Option<String>,
    ) -> Div {
        let Form { s, theme, click } = *self;
        let id = key.replace('.', "-");
        sourced_row(
            s,
            theme,
            key,
            label,
            &|on| {
                select_w(
                    theme,
                    &id,
                    opts,
                    current,
                    click,
                    &|v| if on { cmd(v) } else { Vec::new() },
                    W_LIST,
                )
            },
            note,
        )
    }
}

/// Список (`screen-settings-editor.png`): поле шириной `w` (обычно ≥ 260) с шевроном;
/// варианты `opts` (значение, подпись), клик — следующий по кругу. Один вариант — только
/// показ.
pub(super) fn select_w(
    theme: &Theme,
    id: &str,
    opts: &[(&str, String)],
    current: &str,
    click: ClickAll<'_>,
    cmd: &dyn Fn(&str) -> Vec<Command>,
    w: f32,
) -> Div {
    let at = opts
        .iter()
        .position(|(v, _)| v.eq_ignore_ascii_case(current));
    let label = at.map_or_else(|| current.to_owned(), |i| opts[i].1.clone());
    let next = (opts.len() > 1).then(|| opts[at.map_or(0, |i| (i + 1) % opts.len())].0);
    let field = div()
        .id(SharedString::from(format!("set-{id}")))
        .w(px(w))
        .h(px(28.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.c(Role::StrokeControl))
        .bg(theme.c(Role::FillControl))
        .text_size(px(13.0))
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(SharedString::from(label)),
        )
        .child(
            div()
                .flex_none()
                .font_family("Segoe Fluent Icons")
                .text_size(px(10.0))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::new_static(CHEVRON)),
        );
    div().child(match next {
        Some(v) => field.on_click(click(cmd(v))),
        None => field,
    })
}

/// Подпись кодировки конфига (имя WHATWG в нижнем регистре): полная для списка
/// («Windows-1251»), короткая для кольца F8 («1251»).
fn cp_label(name: &str, short: bool) -> String {
    match name {
        "utf-8" => "UTF-8".into(),
        "ibm866" if short => "866".into(),
        "ibm866" => "CP866".into(),
        n => match n.strip_prefix("windows-") {
            Some(cp) if short => cp.into(),
            Some(cp) => format!("Windows-{cp}"),
            None => n.to_uppercase(),
        },
    }
}

/// Аргумент Fluent одним значением.
fn arg1(
    name: &'static str,
    value: impl Into<urfin_i18n::FluentValue<'static>>,
) -> urfin_i18n::FluentArgs<'static> {
    let mut a = urfin_i18n::FluentArgs::new();
    a.set(name, value);
    a
}

/// «Просмотр» (`screen-settings-viewer.png`, `[viewer]` S7 §8): открытие, текст,
/// большие файлы и логи.
#[allow(clippy::too_many_lines, reason = "форма раздела сверху вниз")]
fn viewer(s: &AppState, theme: &Theme, click: ClickAll<'_>) -> Div {
    use urfin_view::{HexCols, MdView, PlacementCfg, WrapCfg};
    let i18n = urfin_i18n::current();
    let c = s.config.section::<urfin_view::ViewerCfg>("viewer").0;
    let f = Form { s, theme, click };
    let placement = match c.placement {
        PlacementCfg::Window => "window",
        PlacementCfg::Panel => "panel",
    };
    let external = if c.external.is_empty() {
        i18n.get("ui-set-viewer-external-default")
    } else {
        c.external.clone()
    };
    let auto = |cp: &str| {
        if cp == "auto" {
            i18n.get("ui-set-viewer-codepage-auto")
        } else {
            cp_label(cp, false)
        }
    };
    let mut cps: Vec<(&str, String)> = c
        .codepage_ring
        .iter()
        .map(|cp| (cp.as_str(), auto(cp)))
        .collect();
    if !cps
        .iter()
        .any(|(cp, _)| cp.eq_ignore_ascii_case(&c.codepage))
    {
        cps.insert(0, (c.codepage.as_str(), auto(&c.codepage)));
    }
    let ring = c
        .codepage_ring
        .iter()
        .map(|cp| {
            if cp == "auto" {
                i18n.get("ui-set-viewer-codepage-ring-auto")
            } else {
                cp_label(cp, true)
            }
        })
        .collect::<Vec<_>>()
        .join(" → ");
    let wrap = match c.wrap {
        WrapCfg::Off => "off",
        WrapCfg::Words => "words",
        WrapCfg::Chars => "chars",
    };
    let md = match c.markdown {
        MdView::Document => "document",
        MdView::Source => "source",
    };
    let hex = match c.hex_cols {
        HexCols::Auto => "auto".to_owned(),
        HexCols::Fixed(n) => n.to_string(),
    };
    let mb = urfin_view::source::MAP_MIN >> 20;
    let sec = urfin_view::source::MAP_AGE.as_secs();
    div()
        .flex()
        .flex_col()
        .child(group(i18n.get("ui-set-viewer-open")))
        .child(f.seg(
            "viewer.placement",
            i18n.get("ui-set-viewer-placement"),
            &[
                ("window", i18n.get("ui-set-viewer-window")),
                ("panel", i18n.get("ui-set-viewer-panel")),
            ],
            placement,
            Some(i18n.get("ui-set-viewer-placement-note")),
        ))
        .child(f.list(
            "viewer.external",
            i18n.get("ui-set-viewer-external"),
            &[(c.external.as_str(), external)],
            &c.external,
            &|_| Vec::new(),
            Some("Alt+F3".into()),
        ))
        .child(f.flag(
            "viewer.remember_pos",
            i18n.get("ui-set-viewer-remember"),
            c.remember_pos,
            &i18n.get("ui-set-viewer-remember-on"),
            Some(i18n.fmt("ui-set-viewer-remember-note", &arg1("count", c.history_max))),
        ))
        .child(group(i18n.get("ui-set-viewer-text")))
        .child(f.list(
            "viewer.codepage",
            i18n.get("ui-set-viewer-codepage"),
            &cps,
            &c.codepage,
            &|v| set("viewer.codepage", v),
            Some(i18n.fmt("ui-set-viewer-codepage-note", &arg1("ring", ring))),
        ))
        .child(f.seg(
            "viewer.wrap",
            i18n.get("ui-set-viewer-wrap"),
            &[
                ("off", i18n.get("ui-set-viewer-wrap-off")),
                ("words", i18n.get("ui-set-viewer-wrap-words")),
                ("chars", i18n.get("ui-set-viewer-wrap-chars")),
            ],
            wrap,
            None,
        ))
        .child(f.seg(
            "viewer.tab_size",
            i18n.get("ui-set-tab-size"),
            &tab_sizes(),
            &c.tab_size.to_string(),
            None,
        ))
        .child(f.seg(
            "viewer.markdown",
            i18n.get("ui-set-viewer-markdown"),
            &[
                ("document", i18n.get("ui-set-viewer-markdown-document")),
                ("source", i18n.get("ui-set-viewer-markdown-source")),
            ],
            md,
            Some(i18n.get("ui-set-viewer-markdown-note")),
        ))
        .child(f.seg(
            "viewer.hex_cols",
            i18n.get("ui-set-viewer-hex-cols"),
            &[
                ("auto", i18n.get("ui-set-viewer-hex-auto")),
                ("8", "8".into()),
                ("16", "16".into()),
                ("32", "32".into()),
            ],
            &hex,
            Some(i18n.get("ui-set-viewer-hex-note")),
        ))
        .child(group(i18n.get("ui-set-viewer-large")))
        .child(f.flag(
            "viewer.mmap",
            i18n.get("ui-set-viewer-mmap"),
            c.mmap,
            &i18n.fmt("ui-set-viewer-mmap-on", &arg1("mb", mb)),
            Some(i18n.fmt("ui-set-viewer-mmap-note", &arg1("sec", sec))),
        ))
        .child(f.flag(
            "viewer.follow_reopen",
            i18n.get("ui-set-viewer-follow"),
            c.follow_reopen,
            &i18n.get("ui-set-viewer-follow-on"),
            Some(i18n.fmt(
                "ui-set-viewer-follow-note",
                &arg1("ms", c.follow_interval_ms),
            )),
        ))
}

/// Табуляция 2 | 4 | 8 — у просмотра и редактора.
fn tab_sizes() -> [(&'static str, String); 3] {
    [("2", "2".into()), ("4", "4".into()), ("8", "8".into())]
}

/// «Редактор» (`screen-settings-editor.png`, `[editor]` S7 §8): текст и файлы.
#[allow(clippy::too_many_lines, reason = "форма раздела сверху вниз")]
fn editor(s: &AppState, theme: &Theme, click: ClickAll<'_>) -> Div {
    use urfin_view::{Autosave, EolCfg};
    let i18n = urfin_i18n::current();
    let c = s.config.section::<urfin_view::EditorCfg>("editor").0;
    let ui = s.config.section::<crate::config::UiCfg>("ui").0;
    let f = Form { s, theme, click };
    // Пусто — моноширинный шрифт окна (`ui.font_mono`: `system` — Cascadia Mono).
    let family = if !c.font.is_empty() {
        c.font.to_string()
    } else if ui.font_mono.is_empty() || ui.font_mono == "system" {
        "Cascadia Mono".to_owned()
    } else {
        std::path::Path::new(&ui.font_mono).file_stem().map_or_else(
            || ui.font_mono.clone(),
            |n| n.to_string_lossy().into_owned(),
        )
    };
    let size = if c.font_size > 0 {
        f32::from(c.font_size)
    } else {
        ui.font_mono_size
    };
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("family", family);
    a.set("size", format!("{size}"));
    let font = i18n.fmt("ui-set-editor-font-value", &a);
    let new_cp = match (c.new_codepage.as_str(), c.new_bom) {
        ("utf-8", false) => "utf8".to_owned(),
        ("utf-8", true) => "utf8-bom".to_owned(),
        (cp, _) => cp.to_owned(),
    };
    let mut cps = vec![
        ("utf8", i18n.get("ui-set-editor-utf8")),
        ("utf8-bom", i18n.get("ui-set-editor-utf8-bom")),
    ];
    if !new_cp.starts_with("utf8") {
        cps.insert(0, (new_cp.as_str(), cp_label(&new_cp, false)));
    }
    let eol = match c.eol {
        EolCfg::Lf => "lf",
        EolCfg::Crlf => "crlf",
        EolCfg::File => "file",
    };
    let autosave = match c.autosave {
        Autosave::Off => "off",
        Autosave::Focus => "focus",
        Autosave::Delay => "delay",
    };
    let hl_mb = urfin_view::viewer::MARKUP_MAX >> 20;
    div()
        .flex()
        .flex_col()
        .child(group(i18n.get("ui-set-editor-text")))
        .child(f.list(
            "editor.font",
            i18n.get("ui-set-editor-font"),
            &[(c.font.as_str(), font)],
            &c.font,
            &|_| Vec::new(),
            None,
        ))
        .child(f.seg(
            "editor.tab_size",
            i18n.get("ui-set-tab-size"),
            &tab_sizes(),
            &c.tab_size.to_string(),
            None,
        ))
        .child(f.flag(
            "editor.expand_tabs",
            i18n.get("ui-set-editor-indent"),
            c.expand_tabs,
            &i18n.get("ui-set-editor-indent-spaces"),
            None,
        ))
        .child(f.flag(
            "editor.wrap",
            i18n.get("ui-set-editor-wrap"),
            c.wrap,
            &i18n.get("ui-set-editor-wrap-on"),
            None,
        ))
        .child(f.flag(
            "editor.gutter",
            i18n.get("ui-set-editor-gutter"),
            c.gutter,
            &i18n.get("ui-set-editor-gutter-on"),
            None,
        ))
        .child(f.flag(
            "editor.toolbar",
            i18n.get("ui-set-editor-toolbar"),
            c.toolbar,
            &i18n.get("ui-set-editor-toolbar-on"),
            Some(i18n.get("ui-set-editor-toolbar-note")),
        ))
        .child(group(i18n.get("ui-set-editor-files")))
        .child(f.list(
            "editor.new_codepage",
            i18n.get("ui-set-editor-new-cp"),
            &cps,
            &new_cp,
            &|v| {
                let mut cmds = set("editor.new_codepage", "utf-8");
                cmds.extend(set(
                    "editor.new_bom",
                    if v == "utf8-bom" { "true" } else { "false" },
                ));
                cmds
            },
            None,
        ))
        .child(f.seg(
            "editor.eol",
            i18n.get("ui-set-editor-eol"),
            &[
                ("lf", "LF".into()),
                ("crlf", "CRLF".into()),
                ("file", i18n.get("ui-set-editor-eol-file")),
            ],
            eol,
            None,
        ))
        .child(f.flag(
            "editor.highlight",
            i18n.get("ui-set-editor-highlight"),
            c.highlight,
            &i18n.get("ui-set-editor-highlight-on"),
            Some(i18n.fmt("ui-set-editor-highlight-note", &arg1("mb", hl_mb))),
        ))
        .child(f.list(
            "editor.autosave",
            i18n.get("ui-set-editor-autosave"),
            &[
                ("off", i18n.get("ui-set-editor-autosave-off")),
                ("focus", i18n.get("ui-set-editor-autosave-focus")),
                ("delay", i18n.get("ui-set-editor-autosave-delay")),
            ],
            autosave,
            &|v| set("editor.autosave", v),
            None,
        ))
}

/// «Клавиатура»: пресет раскладки.
/// «Операции» (`[ops]`): корзина, конфликт и ошибки по умолчанию, проверка.
#[allow(clippy::too_many_lines, reason = "форма раздела сверху вниз")]
fn ops(s: &AppState, theme: &Theme, click: ClickAll<'_>) -> Div {
    let i18n = urfin_i18n::current();
    let flag = |key: &str, default: bool| cfg_value(s, key).map_or(default, |v| v == "true");
    let trash = flag("ops.delete_to_trash", true);
    let fast = flag("ops.experimental_fast_trash", true);
    let conflict = cfg_value(s, "ops.defaults.conflict").unwrap_or_else(|| "Ask".into());
    let errors = cfg_value(s, "ops.defaults.errors").unwrap_or_else(|| "Ask".into());
    let verify = cfg_value(s, "ops.defaults.verify").unwrap_or_else(|| "Off".into());
    let dirs = cfg_value(s, "ops.defaults.dirs").unwrap_or_else(|| "Merge".into());
    let cloud = cfg_value(s, "ops.defaults.cloud").unwrap_or_else(|| "Hydrate".into());
    let durability = cfg_value(s, "ops.defaults.durability").unwrap_or_else(|| "Fast".into());
    let toggle = |key: &'static str, on: bool| set(key, if on { "false" } else { "true" });
    div()
        .flex()
        .flex_col()
        .child(group(i18n.get("ui-set-ops-delete")))
        .child(policy_row(
            s,
            theme,
            "ops.delete_to_trash",
            i18n.get("ui-set-trash"),
            &|on| {
                div().child(
                    check(theme, "trash", trash, i18n.get("ui-set-trash-on")).when(on, |c| {
                        c.on_click(click(toggle("ops.delete_to_trash", trash)))
                    }),
                )
            },
            Some(i18n.get("ui-set-trash-note")),
        ))
        .child(policy_row(
            s,
            theme,
            "ops.experimental_fast_trash",
            i18n.get("ui-set-fast-trash"),
            &|on| {
                div().child(
                    check(theme, "fast-trash", fast, i18n.get("ui-set-fast-trash-on"))
                        .when(on, |c| {
                            c.on_click(click(toggle("ops.experimental_fast_trash", fast)))
                        }),
                )
            },
            None,
        ))
        .child(group(i18n.get("ui-set-ops-copy")))
        .child(row(
            theme,
            i18n.get("ui-strip-ifexists"),
            segments(
                theme,
                "conflict",
                &[
                    ("Ask", i18n.get("ui-strip-ifexists-ask")),
                    ("Overwrite", i18n.get("ui-strip-ifexists-overwrite")),
                    ("Skip", i18n.get("ui-strip-ifexists-skip")),
                    ("OverwriteIfNewer", i18n.get("ui-strip-ifexists-newer")),
                ],
                &conflict,
                click,
                &|v| set("ops.defaults.conflict", v),
            ),
            None,
        ))
        .child(row(
            theme,
            i18n.get("ui-set-dirs"),
            segments(
                theme,
                "dirs",
                &[
                    ("Merge", i18n.get("ui-set-dirs-merge")),
                    ("Ask", i18n.get("ui-set-errors-ask")),
                    ("Skip", i18n.get("ui-set-errors-skip")),
                ],
                &dirs,
                click,
                &|v| set("ops.defaults.dirs", v),
            ),
            None,
        ))
        .child(row(
            theme,
            i18n.get("ui-set-errors"),
            segments(
                theme,
                "errors",
                &[
                    ("Ask", i18n.get("ui-set-errors-ask")),
                    ("Skip", i18n.get("ui-set-errors-skip")),
                    ("Abort", i18n.get("ui-set-errors-abort")),
                ],
                &errors,
                click,
                &|v| set("ops.defaults.errors", v),
            ),
            None,
        ))
        .child(policy_row(
            s,
            theme,
            "ops.defaults.verify",
            i18n.get("ui-set-verify"),
            &|on| {
                segments(
                    theme,
                    "verify",
                    &[
                        ("Off", i18n.get("ui-set-verify-off")),
                        ("Size", i18n.get("ui-set-verify-size")),
                    ],
                    &verify,
                    click,
                    &|v| {
                        if on {
                            set("ops.defaults.verify", v)
                        } else {
                            Vec::new()
                        }
                    },
                )
            },
            Some(i18n.get("ui-set-verify-note")),
        ))
        .child(row(
            theme,
            i18n.get("ui-set-cloud"),
            segments(
                theme,
                "cloud",
                &[
                    ("Hydrate", i18n.get("ui-set-cloud-hydrate")),
                    ("Skip", i18n.get("ui-set-errors-skip")),
                ],
                &cloud,
                click,
                &|v| set("ops.defaults.cloud", v),
            ),
            None,
        ))
        .child(group(i18n.get("ui-set-durability-group")))
        .child(row(
            theme,
            i18n.get("ui-set-durability"),
            segments(
                theme,
                "durability",
                &[
                    ("Fast", i18n.get("ui-set-durability-fast")),
                    ("Safe", i18n.get("ui-set-durability-safe")),
                ],
                &durability,
                click,
                &|v| set("ops.defaults.durability", v),
            ),
            Some(i18n.get("ui-set-durability-note")),
        ))
}

/// «Политики» — только чтение (ADR-0020, `screen-settings-policies.png`): все ключи;
/// заданные — с замком, значением и источником «GPO · Имя = значение» или
/// «TOML · ключ = значение», незаданные — ключ TOML и «не задано».
fn policies(s: &AppState, theme: &Theme) -> Div {
    let i18n = urfin_i18n::current();
    let set = s.ext.get::<PolicySet>().cloned().unwrap_or_default();
    let line = |name: Div, value: Div, source: String, on: bool| {
        div()
            .h(px(36.0))
            .flex()
            .items_center()
            .border_b_1()
            .border_color(theme.c(Role::StrokeDivider))
            .child(name.w(px(W_LABEL + 20.0)).flex_none().flex().items_center())
            .child(div().w(px(220.0)).flex_none().child(value))
            .child(
                div()
                    .font_family("Cascadia Mono")
                    .text_size(px(12.0))
                    .text_color(theme.c(if on {
                        Role::TextPrimary
                    } else {
                        Role::TextSecondary
                    }))
                    .child(SharedString::from(source)),
            )
    };
    let mut table = div()
        .flex()
        .flex_col()
        .child(group(i18n.get("ui-set-policies-title")).pt(px(0.0)))
        .child(
            div()
                .mt(px(4.0))
                .p(px(12.0))
                .flex()
                .gap(px(10.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(theme.c(Role::StrokeDivider))
                .bg(theme.c(Role::Header))
                .child(lock(theme).ml(px(0.0)).pt(px(2.0)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(SharedString::from(i18n.get("ui-set-policies-note"))),
                ),
        )
        .child(
            div()
                .mt(px(20.0))
                .h(px(28.0))
                .flex()
                .items_center()
                .text_size(px(12.0))
                .text_color(theme.c(Role::TextSecondary))
                .border_b_1()
                .border_color(theme.c(Role::StrokeDivider))
                .child(
                    div()
                        .w(px(W_LABEL + 20.0))
                        .child(SharedString::from(i18n.get("ui-set-policy-param"))),
                )
                .child(
                    div()
                        .w(px(220.0))
                        .child(SharedString::from(i18n.get("ui-set-policy-value"))),
                )
                .child(SharedString::from(i18n.get("ui-set-policy-source"))),
        );
    for key in ORDER {
        let name = div().child(SharedString::from(i18n.get(policy_label(key))));
        table = table.child(match set.get(key) {
            Some(e) => line(
                name.child(lock(theme)),
                div()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .child(SharedString::from(policy_value(key, &e.value))),
                policy_source(key, e.source, &e.value),
                true,
            ),
            None => line(
                name,
                div()
                    .text_color(theme.c(Role::TextSecondary))
                    .child(SharedString::from(i18n.get("ui-set-policy-unset"))),
                key.toml().into(),
                false,
            ),
        });
    }
    table
}

/// Порядок строк «Политик» — как на макете: тема, операции, SSH, анализ места; затем
/// обновление (решение 2026-11-11 «self-update», на макете его нет).
const ORDER: [PolicyKey; 11] = [
    PolicyKey::Theme,
    PolicyKey::ThemeLocked,
    PolicyKey::DeleteToTrash,
    PolicyKey::ExperimentalFastTrash,
    PolicyKey::Verify,
    PolicyKey::PasswordCache,
    PolicyKey::SshRecording,
    PolicyKey::SshMultiExec,
    PolicyKey::DuElevatedHelper,
    PolicyKey::DuElevatedMft,
    PolicyKey::AllowUpdate,
];

/// Подпись ключа политики.
fn policy_label(key: PolicyKey) -> &'static str {
    match key {
        PolicyKey::Theme => "ui-set-policy-theme",
        PolicyKey::ThemeLocked => "ui-set-policy-locked",
        PolicyKey::DeleteToTrash => "ui-set-policy-trash",
        PolicyKey::ExperimentalFastTrash => "ui-set-policy-fast-trash",
        PolicyKey::Verify => "ui-set-policy-verify",
        PolicyKey::PasswordCache => "ui-set-policy-password-cache",
        PolicyKey::SshRecording => "ui-set-policy-ssh-recording",
        PolicyKey::SshMultiExec => "ui-set-policy-multi-exec",
        PolicyKey::DuElevatedHelper => "ui-set-policy-du-helper",
        PolicyKey::DuElevatedMft => "ui-set-policy-du-mft",
        PolicyKey::AllowUpdate => "ui-set-policy-allow-update",
    }
}

/// Значение словами: «Да», «Запрещён», «Обязательна», имя темы.
fn policy_value(key: PolicyKey, v: &PolicyValue) -> String {
    let i18n = urfin_i18n::current();
    match v {
        PolicyValue::Theme(t) => t.to_string(),
        PolicyValue::Bool(b)
            if matches!(key, PolicyKey::DeleteToTrash | PolicyKey::ThemeLocked) =>
        {
            i18n.get(if *b { "ui-set-yes" } else { "ui-set-no" })
        }
        PolicyValue::Bool(b) => {
            // Род слова в русской подписи: корзина, кэш, мультивыполнение, обновление.
            let g = match key {
                PolicyKey::ExperimentalFastTrash => "f",
                PolicyKey::SshMultiExec | PolicyKey::DuElevatedMft | PolicyKey::AllowUpdate => "n",
                _ => "m",
            };
            let mut a = urfin_i18n::FluentArgs::new();
            a.set("g", g);
            let id = if *b {
                "ui-set-policy-allowed"
            } else {
                "ui-set-policy-forbidden"
            };
            i18n.fmt(id, &a)
        }
        PolicyValue::Verify(m) => i18n.get(match m {
            urfin_core::MinVerify::None => "ui-set-verify-off",
            urfin_core::MinVerify::Size => "ui-set-verify-size",
            urfin_core::MinVerify::Hash => "ui-set-verify-hash",
        }),
        PolicyValue::SshRecording(r) => i18n.get(match r {
            urfin_core::SshRecording::Required => "ui-set-policy-rec-required",
            urfin_core::SshRecording::Forbidden => "ui-set-policy-rec-forbidden",
            urfin_core::SshRecording::User => "ui-set-policy-rec-user",
        }),
    }
}

/// Источник как записан: `GPO · DeleteToTrash = 1`, `TOML · ssh.recording = "required"`.
fn policy_source(key: PolicyKey, from: PolicySource, v: &PolicyValue) -> String {
    let value = match (from, v) {
        (PolicySource::Gpo, PolicyValue::Bool(b)) => u8::from(*b).to_string(),
        (_, PolicyValue::Bool(b)) => b.to_string(),
        (PolicySource::Gpo, v) => v.to_string(),
        (PolicySource::Toml, v) => format!("\"{v}\""),
    };
    let name = match from {
        PolicySource::Gpo => key.registry(),
        PolicySource::Toml => key.toml(),
    };
    format!("{from} · {name} = {value}")
}

/// «Доступность»: масштаб и высота строки (те же ключи, что в «Вид»).
fn a11y(s: &AppState, theme: &Theme, click: ClickAll<'_>) -> Div {
    let i18n = urfin_i18n::current();
    let st = s.ext.get::<UiState>();
    let zoom = st.map_or(100, |u| u.zoom);
    let density = st.map_or(24, |u| u.density).to_string();
    let btn = |id: &str, label: String, cmd: Command| {
        super::strip::button(
            theme,
            format!("set-{id}"),
            label,
            "",
            super::strip::Btn::Normal,
        )
        .h(px(30.0))
        .on_click(click(vec![cmd]))
    };
    div()
        .flex()
        .flex_col()
        .child(group(i18n.get("ui-set-visibility")))
        .child(row(
            theme,
            i18n.get("ui-set-zoom"),
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(btn(
                    "zoom-out",
                    "−".into(),
                    Command::new("ui.zoom").arg("delta", -1),
                ))
                .child(
                    div()
                        .w(px(56.0))
                        .flex()
                        .justify_center()
                        .child(SharedString::from(format!("{zoom} %"))),
                )
                .child(btn(
                    "zoom-in",
                    "+".into(),
                    Command::new("ui.zoom").arg("delta", 1),
                ))
                .child(btn(
                    "zoom-reset",
                    i18n.get("ui-set-zoom-reset"),
                    Command::new("ui.zoom"),
                )),
            Some("Ctrl+=, Ctrl+−, Ctrl+0".into()),
        ))
        .child(row(
            theme,
            i18n.get("ui-set-density"),
            segments(
                theme,
                "a11y-density",
                &[
                    ("20", "20 px".into()),
                    ("24", "24 px".into()),
                    ("28", "28 px".into()),
                ],
                &density,
                click,
                &|v| vec![Command::new("ui.density.set").arg("px", v.parse::<i64>().unwrap_or(24))],
            ),
            None,
        ))
}

/// «О программе»: иконка, имя, версия, офлайн, лицензия, папки настроек и журнала.
#[allow(clippy::too_many_lines, reason = "страница сверху вниз")]
fn about(s: &AppState, theme: &Theme, click: ClickAll<'_>) -> Div {
    let i18n = urfin_i18n::current();
    let st = s.ext.get::<UiState>();
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("version", crate::build_version());
    let open = |id: &str, path: &std::path::Path| {
        super::strip::button(
            theme,
            format!("set-open-{id}"),
            i18n.get("ui-set-open"),
            "",
            super::strip::Btn::Normal,
        )
        .h(px(30.0))
        .on_click(click(vec![
            Command::new("ui.settings").arg("close", true),
            Command::new("panel.go").arg("path", urfin_core::Location::local(path)),
        ]))
    };
    let path_line = |id: &str, label: String, path: Option<&std::path::PathBuf>| {
        let control = match path {
            Some(p) => div()
                .flex()
                .items_center()
                .gap(px(12.0))
                .child(
                    div()
                        .font_family("Cascadia Mono")
                        .text_size(px(12.5))
                        .child(SharedString::from(p.display().to_string())),
                )
                .child(open(id, p)),
            None => div()
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from("—")),
        };
        row(theme, label, control, None)
    };
    let icon = gpui_kit::img(std::sync::Arc::new(gpui_kit::Image::from_bytes(
        gpui_kit::ImageFormat::Png,
        ICON.to_vec(),
    )))
    .w(px(80.0))
    .h(px(80.0));
    div()
        .flex()
        .flex_col()
        .h_full()
        .child(
            div()
                .pt(px(8.0))
                .pb(px(24.0))
                .flex()
                .items_center()
                .gap(px(20.0))
                .child(icon)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .child(
                            div()
                                .text_size(px(22.0))
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .child(SharedString::from(i18n.get("ui-set-app-name"))),
                        )
                        .child(
                            div()
                                .text_color(theme.c(Role::TextSecondary))
                                .child(SharedString::from(i18n.fmt("ui-set-version", &a))),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .font_family("Segoe Fluent Icons")
                                        .text_color(theme.c(Role::AccentText))
                                        .child(SharedString::new_static("\u{E753}")),
                                )
                                .child(SharedString::from(i18n.get("ui-set-offline"))),
                        ),
                ),
        )
        .child(row(
            theme,
            i18n.get("ui-set-license"),
            div()
                .font_family("Cascadia Mono")
                .text_size(px(12.5))
                .child(SharedString::from(i18n.get("ui-set-license-value"))),
            None,
        ))
        .child(path_line(
            "config",
            i18n.get("ui-set-config-dir"),
            st.and_then(|u| u.config_dir.as_ref()),
        ))
        .child(path_line(
            "logs",
            i18n.get("ui-set-log-dir"),
            st.and_then(|u| u.log_dir.as_ref()),
        ))
        .child(updates(s, theme, click))
        .child(div().flex_1())
        .child(
            div()
                .pt(px(12.0))
                .border_t_1()
                .border_color(theme.c(Role::StrokeDivider))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(format!(
                    "© 2026 {}",
                    i18n.get("ui-set-author")
                ))),
        )
}

/// «Обновления» в «О программе» (решение 2026-11-11 «self-update»; на макете
/// `screen-about` их нет — по образцу «Пакетов»): «Проверять раз в сутки» и «Проверить
/// сейчас», канал «Стабильный / Бета». Политика `allow_update = false` — выключатель
/// заперт, кнопки нет; пояснение справа — итог проверки.
fn updates(s: &AppState, theme: &Theme, click: ClickAll<'_>) -> Div {
    use crate::update::{CHANNEL_KEY, CHECK_KEY, Status, UpdateCfg};
    let i18n = urfin_i18n::current();
    let m = crate::update::model(s);
    let cfg = UpdateCfg::from_config(&s.config);
    let on = cfg.check;
    let locked = !m.allowed || locked(s, CHECK_KEY);
    let mut a = urfin_i18n::FluentArgs::new();
    let note = match &m.status {
        _ if !m.keyed => Some(i18n.get("ui-update-no-key")),
        Status::Idle => None,
        Status::Checking => Some(i18n.get("ui-update-checking")),
        Status::UpToDate => Some(i18n.get("ui-update-latest")),
        Status::Available(v) | Status::Installing(v) => {
            a.set("version", v.clone());
            Some(i18n.fmt("ui-update-offer", &a))
        }
        Status::Failed(e) => {
            a.set("error", e.clone());
            Some(i18n.fmt("ui-update-check-failed", &a))
        }
    };
    let busy = matches!(m.status, Status::Checking | Status::Installing(_));
    let control = |enabled: bool| {
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.0))
            .child(
                check(theme, "update-check", on, i18n.get("ui-set-update-daily"))
                    .when(enabled, |c| {
                        c.on_click(click(set(CHECK_KEY, if on { "false" } else { "true" })))
                    }),
            )
            .when(m.allowed, |d| {
                d.child(
                    super::strip::button(
                        theme,
                        "set-update-check-now".into(),
                        i18n.get("ui-set-update-check-now"),
                        "",
                        super::strip::Btn::Normal,
                    )
                    .ml(px(8.0))
                    .h(px(28.0))
                    .when(!busy, |b| {
                        b.on_click(click(vec![Command::new("update.check")]))
                    }),
                )
            })
    };
    let check_row = if locked {
        locked_row(
            theme,
            i18n.get("ui-set-update"),
            &control,
            i18n.get("ui-set-by-policy"),
        )
    } else {
        row(theme, i18n.get("ui-set-update"), control(true), note)
    };
    let beta = cfg.beta(crate::version());
    div()
        .flex()
        .flex_col()
        .child(group(i18n.get("ui-set-updates")))
        .child(check_row)
        .child(row(
            theme,
            i18n.get("ui-set-update-channel"),
            segments(
                theme,
                "update-channel",
                &[
                    ("stable", i18n.get("ui-set-update-stable")),
                    ("beta", i18n.get("ui-set-update-beta")),
                ],
                if beta { "beta" } else { "stable" },
                click,
                &|v| set(CHANNEL_KEY, v),
            ),
            None,
        ))
}

/// Следующий или предыдущий раздел (Tab, ↑↓).
pub(super) fn step(section: &str, dir: isize) -> &'static str {
    let n = SECTIONS.len();
    let i = SECTIONS
        .iter()
        .position(|(id, _)| *id == section)
        .unwrap_or(0);
    #[allow(
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss,
        reason = "одиннадцать разделов"
    )]
    let j = ((i as isize + dir).rem_euclid(n as isize)) as usize;
    SECTIONS[j].0
}

/// Вкладка настроек как фича окна (ADR-0022): открыта — на месте панелей.
#[derive(Default)]
pub(super) struct Settings {
    /// Поля ввода раздела «SSH».
    fields: Fields,
    /// Поля ввода раздела «Пакеты».
    pkg: PkgFields,
    /// Поиск и прокрутка раздела «Клавиатура».
    pub(super) keys: super::settings_keys::Page,
}

impl super::features::ShellFeature for Settings {
    /// Tab, ↑↓ — раздел, Esc — закрыть; остальные клавиши — не панелям.
    fn key(&mut self, s: &AppState, ks: &gpui_kit::Keystroke) -> Option<Vec<Command>> {
        let sec = s.ext.get::<UiState>()?.settings.as_deref()?;
        if sec == "conn"
            && let Some(c) = conn_key(s, ks)
        {
            return Some(c);
        }
        let dir = match ks.key.as_str() {
            "tab" if ks.modifiers.shift => Some(-1),
            "up" => Some(-1),
            "tab" | "down" => Some(1),
            _ => None,
        };
        let cmd = match (ks.key.as_str(), dir) {
            ("escape", _) => Some(Command::new("ui.settings").arg("close", true)),
            (_, Some(d)) => Some(Command::new("ui.settings").arg("section", step(sec, d))),
            _ => None,
        };
        Some(cmd.into_iter().collect())
    }

    /// Поле раздела с фокусом: Enter и Tab применяют, Esc — возвращает прежнее значение,
    /// остальное — полю.
    fn key_in(
        &mut self,
        s: &AppState,
        ks: &gpui_kit::Keystroke,
        window: &mut gpui_kit::Window,
        cx: &mut gpui_kit::Context<super::CoreHost>,
    ) -> Option<Vec<Command>> {
        if self.pkg.editing() {
            return self.pkg.key(s, ks, window, cx);
        }
        let keys_open = s.ext.get::<UiState>().and_then(|u| u.settings.as_deref()) == Some("keys");
        if keys_open && let Some(c) = self.keys.key(s, ks, window, cx) {
            return Some(c);
        }
        if keys_open && self.keys.editing() {
            return None;
        }
        if !self.fields.editing() {
            return self.key(s, ks);
        }
        match ks.key.as_str() {
            "enter" | "tab" => self.fields.leave(),
            "escape" => self.fields.cancel(s, window, cx),
            _ => return None,
        }
        cx.notify();
        Some(Vec::new())
    }

    fn editing(&self, _s: &AppState) -> bool {
        self.fields.editing() || self.pkg.editing() || self.keys.editing()
    }

    fn input(
        &mut self,
        s: &AppState,
        focus: &gpui_kit::FocusHandle,
        window: &mut gpui_kit::Window,
        cx: &mut gpui_kit::Context<super::CoreHost>,
    ) {
        self.fields.sync(s, focus, window, cx);
        self.pkg.sync(s, focus, window, cx);
        self.keys.sync(s, focus, window, cx);
    }

    fn main(
        &mut self,
        f: &super::features::Frame<'_>,
        _hidden: bool,
        window: &mut gpui_kit::Window,
        cx: &gpui_kit::Context<super::CoreHost>,
    ) -> Option<super::features::Main> {
        let sec = f.s.ext.get::<UiState>()?.settings.as_deref()?;
        let win_h = f32::from(window.viewport_size().height);
        let keys = (sec == "keys").then(|| self.keys.section(f.s, f.theme, f.click_all, win_h, cx));
        let fields = (&self.fields, &self.pkg);
        Some(super::features::Main::Replace(
            view(f.s, sec, f.theme, f.click_all, fields, keys, cx).into_any_element(),
        ))
    }
}

/// «Подключения» (`screen-settings-conn`): ↑↓ — профиль, буквы — поиск, Enter —
/// подключиться, Ins/F4/Del — добавить, изменить, удалить; Tab и Esc — как у разделов.
fn conn_key(s: &AppState, ks: &gpui_kit::Keystroke) -> Option<Vec<Command>> {
    let u = s.ext.get::<UiState>()?;
    let rows = crate::ssh::conn_rows(s, &u.conn_filter);
    let n = rows.len();
    let sel = u.conn_sel.min(n.saturating_sub(1));
    let cur = rows.get(sel).map(|p| p.name.clone());
    let side = crate::side_arg(s.active);
    let pick = |i: usize| {
        vec![
            Command::new("ui.settings")
                .arg("section", "conn")
                .arg("sel", i64::try_from(i).unwrap_or(0)),
        ]
    };
    let filter = |f: String| {
        vec![
            Command::new("ui.settings")
                .arg("section", "conn")
                .arg("filter", f),
        ]
    };
    let close = || Command::new("ui.settings").arg("close", true);
    Some(match ks.key.as_str() {
        "up" if n > 0 => pick((sel + n - 1) % n),
        "down" if n > 0 => pick((sel + 1) % n),
        "up" | "down" => Vec::new(),
        "enter" if ks.modifiers.shift => match cur {
            Some(p) => vec![close(), Command::new("ssh.terminal").arg("profile", p)],
            None => Vec::new(),
        },
        "enter" => match cur {
            Some(p) => vec![
                close(),
                Command::new("sftp.open")
                    .arg("profile", p)
                    .arg("side", side),
            ],
            None => Vec::new(),
        },
        "insert" => vec![close(), Command::new("ssh.new").arg("side", side)],
        "f4" => match cur {
            Some(p) => vec![
                close(),
                Command::new("ssh.edit").arg("profile", p).arg("side", side),
            ],
            None => Vec::new(),
        },
        "delete" => cur.map_or_else(Vec::new, |p| {
            vec![Command::new("ssh.remove").arg("profile", p)]
        }),
        "backspace" if !u.conn_filter.is_empty() => {
            let mut f = u.conn_filter.to_string();
            f.pop();
            filter(f)
        }
        "escape" if !u.conn_filter.is_empty() => filter(String::new()),
        _ => {
            let ch = ks.key_char.as_deref().filter(|c| {
                !ks.modifiers.control && !ks.modifiers.alt && c.chars().all(|c| !c.is_control())
            })?;
            filter(format!("{}{ch}", u.conn_filter))
        }
    })
}

/// Сцены вкладки настроек (D-08): разделы.
pub(super) const SCENES: &[Scene] = &[
    Scene {
        name: "screen-settings",
        steps: &["ui.cancel", "ui.settings section=view"],
    },
    Scene {
        name: "screen-settings-panels",
        steps: &["ui.settings section=panels"],
    },
    // «Клавиатура»: на макете сразу захват и конфликт — демо пишет конфликт в `user.toml`
    // песочницы; сброс — сразу за снимком.
    Scene {
        name: "settings-keys-seed",
        steps: &[
            "resize 1280x760",
            "ui.settings section=keys",
            "screens.keys",
        ],
    },
    Scene {
        name: "screen-settings-keys",
        steps: &["ui.keys sel=5", "ui.keys capture=replace"],
    },
    Scene {
        name: "settings-keys-ask",
        steps: &["ui.keys chord=F7"],
    },
    Scene {
        name: "settings-keys-found",
        steps: &[
            "ui.keys cancel=true",
            "keymap.reset",
            "ui.keys filter=ctrl+f",
        ],
    },
    Scene {
        name: "screen-settings-editor",
        steps: &["ui.settings section=editor"],
    },
    Scene {
        name: "screen-settings-viewer",
        steps: &["ui.settings section=viewer"],
    },
    Scene {
        name: "screen-settings-ops",
        steps: &["ui.settings section=ops"],
    },
    Scene {
        name: "screen-settings-policies",
        steps: &["resize 1280x760", "ui.settings section=policies"],
    },
    Scene {
        name: "screen-settings-a11y",
        steps: &["resize 1200x760", "ui.settings section=a11y"],
    },
    Scene {
        name: "screen-about",
        steps: &["ui.about"],
    },
    // Вышла новая версия (решение 2026-11-11 «self-update»): баннер внизу и итог проверки
    // в «Обновлениях».
    Scene {
        name: "about-update",
        steps: &["screens.update"],
    },
    Scene {
        name: "settings-closed",
        steps: &["screens.update stop=true", "ui.settings close=true"],
    },
];

#[cfg(test)]
mod tests {
    use super::step;

    #[test]
    fn sections_wrap() {
        assert_eq!(step("view", 1), "panels");
        assert_eq!(step("viewer", 1), "ssh");
        assert_eq!(step("ssh", 1), "conn");
        assert_eq!(step("view", -1), "about");
        assert_eq!(step("about", 1), "view");
        assert_eq!(step("nope", 1), "panels");
    }
}
