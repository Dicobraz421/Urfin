//! Раздел настроек «SSH» (F03-14, `screen-settings-ssh.png`, `[ssh]` F03 §2.9,
//! `[term.highlight]` §3.3): вход, пароли, сеансы, подсветка вывода. Переключатели — как
//! в остальных разделах (`ui.setting`, сразу); числа и команда — поля ввода: значение
//! применяется при уходе из поля (Enter, Tab, клик мимо), Esc — вернуть прежнее.
//! Настройка под политикой — замок и источник TOML/GPO.

use gpui_kit::component::Sizable as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    Context, Div, Entity, FocusHandle, Hsla, SharedString, Subscription, Window, div, px,
};
use std::cell::Cell;
use std::rc::Rc;
use urfin_core::net::{AgentSeen, NetModel, PasswordManager, SshCfg};
use urfin_core::{AppState, Command, PolicyKey, PolicySet, SshRecording};

use super::CoreHost;
use super::overlays::ClickAll;
use super::settings::{Form, check, group, row, set};
use super::theme::{Theme, hsla};
use crate::UiState;
use crate::tokens::Role;

const MONO: &str = "Cascadia Mono";
/// Поле числа и команды (`screen-settings-ssh.png`).
const W_NUM: f32 = 44.0;
const W_CMD: f32 = 300.0;
const W_KEY: f32 = 260.0;

/// Поля раздела: что вводится и куда пишется.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    Tries,
    Interval,
    Keepalive,
    Command,
    /// Новый ключ по умолчанию («Добавить»).
    AddKey,
}

const FIELDS: [Key; 4] = [Key::Tries, Key::Interval, Key::Keepalive, Key::Command];

/// Текст поля по настройкам. Команда: своя или шаблон менеджера.
fn value(key: Key, c: &SshCfg) -> String {
    match key {
        Key::Tries => c.reconnect.tries.to_string(),
        Key::Interval => c.reconnect.interval_s.to_string(),
        Key::Keepalive => c.keepalive_s.to_string(),
        Key::Command if c.password_cmd.is_empty() => {
            c.password_manager.template().unwrap_or_default().to_owned()
        }
        Key::Command => c.password_cmd.clone(),
        Key::AddKey => String::new(),
    }
}

/// `ui.setting` строкой: `123` и `true` остаются текстом (команда пароля).
fn set_text(key: &str, text: &str) -> Command {
    Command::new("ui.setting")
        .arg("key", key)
        .arg("value", text)
        .arg("text", true)
}

/// Команды применения текста поля; не изменилось или не разобрано — пусто (поле вернётся
/// к настройке).
fn commit(key: Key, text: &str, c: &SshCfg) -> Vec<Command> {
    let text = text.trim();
    if key != Key::AddKey && text == value(key, c) {
        return Vec::new();
    }
    let number = |k: &str| {
        text.parse::<u32>()
            .map_or_else(|_| Vec::new(), |n| set(k, &n.to_string()))
    };
    match key {
        Key::Tries => number("ssh.reconnect.tries"),
        Key::Interval => number("ssh.reconnect.interval_s"),
        Key::Keepalive => number("ssh.keepalive_s"),
        // Текст шаблона — снова шаблон менеджера; своя команда — менеджер «Другая».
        Key::Command if text.is_empty() || c.password_manager.template() == Some(text) => {
            vec![set_text("ssh.password_cmd", "")]
        }
        Key::Command => {
            let mut out = vec![set_text("ssh.password_cmd", text)];
            if c.password_manager != PasswordManager::Custom {
                out.extend(set("ssh.password_manager", PasswordManager::Custom.id()));
            }
            out
        }
        Key::AddKey if text.is_empty() || c.default_keys.iter().any(|k| k == text) => Vec::new(),
        Key::AddKey => {
            let list: Vec<urfin_core::Value> = c
                .default_keys
                .iter()
                .map(String::as_str)
                .chain([text])
                .map(|k| urfin_core::Value::Str(k.into()))
                .collect();
            vec![
                Command::new("ui.setting")
                    .arg("key", "ssh.default_keys")
                    .arg("list", list),
            ]
        }
    }
}

/// Поле ввода раздела.
struct Field {
    key: Key,
    input: Entity<InputState>,
    /// Фокус в поле: зеркало событий, `editing` окна и контекста не получает.
    focused: Rc<Cell<bool>>,
    _sub: Subscription,
}

impl Field {
    fn new(key: Key, init: String, window: &mut Window, cx: &mut Context<CoreHost>) -> Field {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(init));
        let focused = Rc::new(Cell::new(false));
        let mirror = Rc::clone(&focused);
        let sub = cx.subscribe_in(
            &input,
            window,
            move |h: &mut CoreHost, input, ev: &InputEvent, window, cx| match ev {
                InputEvent::Focus => {
                    mirror.set(true);
                    cx.notify();
                }
                InputEvent::Blur => {
                    mirror.set(false);
                    let text = input.read(cx).value().to_string();
                    let mut cmds = h
                        .core
                        .as_ref()
                        .map_or_else(Vec::new, |core| commit(key, &text, &cfg(core.state())));
                    // Строка «Добавить» закрывается с уходом из поля.
                    if key == Key::AddKey {
                        cmds.push(Command::new("ui.settings").arg("section", "ssh"));
                    }
                    for c in cmds {
                        h.run(c, window, cx);
                    }
                    cx.notify();
                }
                _ => {}
            },
        );
        Field {
            key,
            input,
            focused,
            _sub: sub,
        }
    }

    fn text(&self, cx: &Context<CoreHost>) -> String {
        self.input.read(cx).value().to_string()
    }

    fn show(&self, text: String, window: &mut Window, cx: &mut Context<CoreHost>) {
        self.input
            .update(cx, |st, cx| st.set_value(text, window, cx));
    }
}

/// Поля раздела: живут, пока он открыт.
#[derive(Default)]
pub(super) struct Fields {
    list: Vec<Field>,
    add: Option<Field>,
    /// Вернуть фокус окну в следующем кадре (там есть его `FocusHandle`).
    blur: bool,
}

impl Fields {
    pub(super) fn editing(&self) -> bool {
        self.list.iter().chain(&self.add).any(|f| f.focused.get())
    }

    /// Enter, Tab — уйти из поля: значение применит `Blur`.
    pub(super) fn leave(&mut self) {
        self.blur = true;
    }

    /// Esc: вернуть значение настройки и уйти из поля.
    pub(super) fn cancel(&mut self, s: &AppState, window: &mut Window, cx: &mut Context<CoreHost>) {
        let c = cfg(s);
        for f in self.list.iter().chain(&self.add) {
            if f.focused.get() {
                f.show(value(f.key, &c), window, cx);
            }
        }
        self.blur = true;
    }

    /// Поля под состояние, в кадре: раздел закрыт — без полей (поле с фокусом сперва
    /// отдаёт значение); настройка сменилась не из поля — новый текст.
    pub(super) fn sync(
        &mut self,
        s: &AppState,
        focus: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) {
        let ui = s.ext.get::<UiState>();
        if ui.and_then(|u| u.settings.as_deref()) != Some("ssh") {
            if self.editing() {
                window.focus(focus, cx);
            } else {
                self.list.clear();
                self.add = None;
                self.blur = false;
            }
            return;
        }
        let c = cfg(s);
        if self.list.is_empty() {
            self.list = FIELDS
                .into_iter()
                .map(|k| Field::new(k, value(k, &c), window, cx))
                .collect();
        }
        for f in &self.list {
            let v = value(f.key, &c);
            if !f.focused.get() && f.text(cx) != v {
                f.show(v, window, cx);
            }
        }
        match (ui.is_some_and(|u| u.ssh_add_key), &self.add) {
            (true, None) => {
                let f = Field::new(Key::AddKey, String::new(), window, cx);
                f.input.update(cx, |st, cx| st.focus(window, cx));
                self.add = Some(f);
            }
            (false, Some(_)) => {
                self.add = None;
                self.blur = false;
            }
            _ => {}
        }
        if self.blur {
            self.blur = false;
            window.focus(focus, cx);
        }
    }

    fn field(&self, key: Key) -> Option<&Field> {
        self.list.iter().chain(&self.add).find(|f| f.key == key)
    }
}

fn cfg(s: &AppState) -> SshCfg {
    s.config.section::<SshCfg>("ssh").0
}

/// Поле в рамке шириной `w`, моноширинное.
fn input_box(theme: &Theme, f: Option<&Field>, w: f32) -> Div {
    let focused = f.is_some_and(|f| f.focused.get());
    let b = div()
        .flex_none()
        .w(px(w))
        .h(px(28.0))
        .flex()
        .items_center()
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.c(if focused {
            Role::AccentFill
        } else {
            Role::StrokeControl
        }))
        .bg(theme.c(Role::FillControlInput))
        .font_family(MONO)
        .text_size(px(12.5));
    match f {
        Some(f) => b.child(Input::new(&f.input).small().appearance(false)),
        None => b,
    }
}

fn secondary(theme: &Theme, text: String) -> Div {
    div()
        .flex_none()
        .text_color(theme.c(Role::TextSecondary))
        .child(SharedString::from(text))
}

fn mono(text: String) -> Div {
    div()
        .flex_none()
        .font_family(MONO)
        .text_size(px(12.5))
        .child(SharedString::from(text))
}

/// Подстановка команды: `{host}` в рамке.
fn token(theme: &Theme, text: &'static str) -> Div {
    div()
        .flex_none()
        .h(px(20.0))
        .px(px(5.0))
        .flex()
        .items_center()
        .rounded(px(3.0))
        .border_1()
        .border_color(theme.c(Role::StrokeDivider))
        .bg(theme.c(Role::Header))
        .font_family(MONO)
        .text_size(px(11.5))
        .text_color(theme.c(Role::TextSecondary))
        .child(SharedString::new_static(text))
}

/// Правило подсветки: чип с цветом роли (`screen-settings-ssh.png`).
fn chip(theme: &Theme, id: &str, label: String, color: Hsla, on: bool) -> gpui_kit::Stateful<Div> {
    div()
        .id(SharedString::from(format!("set-rule-{id}")))
        .flex_none()
        .h(px(26.0))
        .px(px(10.0))
        .flex()
        .items_center()
        .gap(px(7.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.c(if on {
            Role::AccentFill
        } else {
            Role::StrokeControl
        }))
        .when(on, |d| d.bg(theme.c(Role::FillCursor)))
        .text_size(px(12.0))
        .text_color(theme.c(if on {
            Role::TextPrimary
        } else {
            Role::TextSecondary
        }))
        .child(
            div()
                .flex_none()
                .size(px(8.0))
                .rounded(px(2.0))
                .bg(color)
                .when(!on, |d| d.opacity(0.45)),
        )
        .child(SharedString::from(label))
}

/// Пояснение у «Агента»: что найдено на машине (F03 §2.5).
fn agent_note(s: &AppState) -> String {
    let i18n = urfin_i18n::current();
    let tail = i18n.get("net-set-agent-via");
    let found = match s.ext.get::<NetModel>().and_then(|m| m.agent) {
        Some(AgentSeen::OpenSsh) => i18n.get("net-set-agent-openssh"),
        Some(AgentSeen::Pageant) => i18n.get("net-set-agent-pageant"),
        Some(AgentSeen::Missing) => i18n.get("net-set-agent-missing"),
        None => return tail,
    };
    format!("{found} · {tail}")
}

/// Папка записей с раскрытыми `%VAR%`: для «Открыть».
fn recordings_dir(c: &SshCfg) -> String {
    urfin_term::rec::expand_env(&c.recordings_dir, |n| std::env::var(n).ok())
}

/// Раздел формой сверху вниз.
#[allow(clippy::too_many_lines, reason = "форма раздела сверху вниз")]
pub(super) fn section(s: &AppState, theme: &Theme, click: ClickAll<'_>, fields: &Fields) -> Div {
    let i18n = urfin_i18n::current();
    let c = cfg(s);
    let f = Form { s, theme, click };
    let policy = s.ext.get::<PolicySet>();
    let cache = policy
        .and_then(|p| p.bool(PolicyKey::PasswordCache))
        .unwrap_or(c.password_cache);
    let rec = match policy.and_then(PolicySet::ssh_recording) {
        Some(SshRecording::Required) => "always",
        Some(SshRecording::Forbidden) => "off",
        _ => c.recording.id(),
    };
    let hl = s
        .ext
        .get::<urfin_term::TermState>()
        .map(|t| t.config.highlight.clone())
        .unwrap_or_default();
    let flip = |on: bool| if on { "false" } else { "true" };
    let adding = s.ext.get::<UiState>().is_some_and(|u| u.ssh_add_key);
    let button = |id: &str, label: String, cmd: Command| {
        super::strip::button(
            theme,
            format!("set-{id}"),
            label,
            "",
            super::strip::Btn::Normal,
        )
        .h(px(28.0))
        .on_click(click(vec![cmd]))
    };
    let mut keys = div().flex().flex_wrap().items_center().gap(px(12.0));
    for k in &c.default_keys {
        keys = keys.child(mono(k.clone()));
    }
    keys = if adding {
        keys.child(input_box(theme, fields.field(Key::AddKey), W_KEY))
    } else {
        keys.child(button(
            "add-key",
            i18n.get("net-set-key-add"),
            Command::new("ui.settings")
                .arg("section", "ssh")
                .arg("add_key", true),
        ))
    };
    let managers: Vec<(&str, String)> = PasswordManager::ALL
        .into_iter()
        .map(|m| {
            let label = match m {
                PasswordManager::None => i18n.get("net-set-pm-none"),
                PasswordManager::BearPass => "BearPass".into(),
                PasswordManager::Passwork => i18n.get("net-pm-passwork"),
                PasswordManager::OnePassword => "1Password".into(),
                PasswordManager::Bitwarden => "Bitwarden".into(),
                PasswordManager::KeePassXc => "KeePassXC".into(),
                PasswordManager::Custom => i18n.get("net-set-pm-custom"),
            };
            (m.id(), label)
        })
        .collect();
    let num = |key: Key, pre: Option<String>, post: Option<String>| {
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.0))
            .children(pre.map(|t| secondary(theme, t)))
            .child(input_box(theme, fields.field(key), W_NUM))
            .children(post.map(|t| secondary(theme, t)))
    };
    let seconds = || Some(i18n.get("net-set-seconds"));
    let syntax = &theme.tokens.theme.syntax;
    let rules = [
        (
            "errors",
            "net-set-rule-errors",
            theme.c(Role::Modified),
            hl.rules.errors,
        ),
        (
            "warnings",
            "net-set-rule-warnings",
            theme.c(Role::ContextRemote),
            hl.rules.warnings,
        ),
        (
            "paths",
            "net-set-rule-paths",
            hsla(syntax.keyword),
            hl.rules.paths,
        ),
        (
            "urls",
            "net-set-rule-urls",
            theme.c(Role::AccentFill),
            hl.rules.urls,
        ),
        ("ip", "net-set-rule-ip", hsla(syntax.number), hl.rules.ip),
        (
            "dates",
            "net-set-rule-dates",
            hsla(syntax.string),
            hl.rules.dates,
        ),
        (
            "hashes",
            "net-set-rule-hashes",
            hsla(syntax.comment),
            hl.rules.hashes,
        ),
    ];
    let mut chips = div().flex().flex_wrap().items_center().gap(px(6.0));
    for (id, label, color, on) in rules {
        chips = chips.child(
            chip(theme, id, i18n.get(label), color, on)
                .on_click(click(set(&format!("term.highlight.rules.{id}"), flip(on)))),
        );
    }
    div()
        .flex()
        .flex_col()
        .child(group(i18n.get("net-set-login")))
        .child(f.seg(
            "ssh.agent",
            i18n.get("net-set-agent"),
            &[
                ("auto", i18n.get("net-set-agent-auto")),
                ("openssh", "OpenSSH".into()),
                ("pageant", "Pageant".into()),
            ],
            c.agent.id(),
            Some(agent_note(s)),
        ))
        .child(row(
            theme,
            i18n.get("net-set-keys"),
            keys,
            Some("Ed25519, ECDSA, RSA".into()),
        ))
        .child(row(
            theme,
            i18n.get("net-set-import"),
            div()
                .flex()
                .gap(px(24.0))
                .child(
                    check(
                        theme,
                        "ssh-import-config",
                        c.import.ssh_config,
                        "~/.ssh/config".into(),
                    )
                    .on_click(click(set(
                        "ssh.import.ssh_config",
                        flip(c.import.ssh_config),
                    ))),
                )
                .child(
                    check(
                        theme,
                        "ssh-import-putty",
                        c.import.putty,
                        i18n.get("net-set-import-putty"),
                    )
                    .on_click(click(set("ssh.import.putty", flip(c.import.putty)))),
                ),
            Some(i18n.get("net-set-import-note")),
        ))
        .child(group(i18n.get("net-set-passwords")))
        .child(f.flag(
            "ssh.password_cache",
            i18n.get("net-set-cache"),
            cache,
            &i18n.get("net-set-cache-on"),
            None,
        ))
        .child(f.list(
            "ssh.password_manager",
            i18n.get("net-set-manager"),
            &managers,
            c.password_manager.id(),
            &|v| {
                let mut cmds = set("ssh.password_manager", v);
                // Свой текст команды сильнее шаблона: выбор менеджера его снимает.
                if v != PasswordManager::Custom.id() {
                    cmds.push(set_text("ssh.password_cmd", ""));
                }
                cmds
            },
            Some(i18n.get("net-set-manager-note")),
        ))
        .child(row(
            theme,
            i18n.get("net-set-command"),
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(input_box(theme, fields.field(Key::Command), W_CMD))
                .child(secondary(theme, i18n.get("net-set-command-vars")).ml(px(8.0)))
                .child(token(theme, "{host}"))
                .child(token(theme, "{user}"))
                .child(token(theme, "{id}")),
            Some(i18n.get("net-set-command-note")),
        ))
        .child(group(i18n.get("net-set-sessions")))
        .child(row(
            theme,
            i18n.get("net-set-reconnect"),
            div()
                .flex()
                .items_center()
                .gap(px(16.0))
                .child(num(Key::Tries, Some(i18n.get("net-set-tries")), None))
                .child(num(
                    Key::Interval,
                    Some(i18n.get("net-set-interval")),
                    seconds(),
                )),
            Some(i18n.get("net-set-reconnect-note")),
        ))
        .child(row(
            theme,
            "Keep-alive".into(),
            div().flex().child(num(Key::Keepalive, None, seconds())),
            Some(i18n.get("net-set-keepalive-note")),
        ))
        .child(f.seg(
            "ssh.recording",
            i18n.get("net-set-recording"),
            &[
                ("off", i18n.get("net-set-recording-off")),
                ("manual", i18n.get("net-set-recording-manual")),
                ("always", i18n.get("net-set-recording-always")),
            ],
            rec,
            None,
        ))
        .child(row(
            theme,
            i18n.get("net-set-rec-dir"),
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(mono(c.recordings_dir.clone()))
                .child(button(
                    "rec-dir",
                    i18n.get("ui-set-open"),
                    Command::new("ui.settings").arg("open_dir", recordings_dir(&c)),
                )),
            Some(i18n.get("net-set-rec-dir-note")),
        ))
        .child(f.flag(
            "ssh.multi_exec_confirm",
            i18n.get("net-set-multi"),
            c.multi_exec_confirm,
            &i18n.get("net-set-multi-on"),
            Some(i18n.get("net-set-multi-note")),
        ))
        .child(group(i18n.get("net-set-highlight")))
        .child(f.flag(
            "term.highlight.enabled",
            i18n.get("net-set-highlight-row"),
            hl.enabled,
            &i18n.get("net-set-highlight-on"),
            Some(i18n.get("net-set-highlight-note")),
        ))
        .child(row(theme, i18n.get("net-set-rules"), chips, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(manager: PasswordManager, cmd: &str) -> SshCfg {
        SshCfg {
            password_manager: manager,
            password_cmd: cmd.into(),
            ..SshCfg::default()
        }
    }

    fn pairs(c: &[Command]) -> Vec<(String, String)> {
        c.iter()
            .map(|c| {
                (
                    c.args.str("key").unwrap_or_default().to_owned(),
                    c.args.str("value").unwrap_or_default().to_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn numbers_write_only_valid_changes() {
        let c = SshCfg::default();
        assert_eq!(
            pairs(&commit(Key::Keepalive, " 60 ", &c)),
            [("ssh.keepalive_s".to_owned(), "60".to_owned())]
        );
        assert!(commit(Key::Keepalive, "30", &c).is_empty(), "без изменений");
        assert!(commit(Key::Tries, "-1", &c).is_empty());
        assert!(commit(Key::Interval, "abc", &c).is_empty());
    }

    #[test]
    fn command_field_and_manager_template() {
        let bear = with(PasswordManager::BearPass, "");
        let tpl = value(Key::Command, &bear);
        assert_eq!(Some(tpl.as_str()), PasswordManager::BearPass.template());
        assert!(commit(Key::Command, &tpl, &bear).is_empty());
        // Своя команда — менеджер «Другая»; текст шаблона её снимает.
        assert_eq!(
            pairs(&commit(Key::Command, "mine {host}", &bear)),
            [
                ("ssh.password_cmd".to_owned(), "mine {host}".to_owned()),
                ("ssh.password_manager".to_owned(), "custom".to_owned()),
            ]
        );
        let own = with(PasswordManager::BearPass, "mine");
        assert_eq!(
            pairs(&commit(Key::Command, &tpl, &own)),
            [("ssh.password_cmd".to_owned(), String::new())]
        );
    }

    #[test]
    fn add_key_appends_once() {
        let c = SshCfg::default();
        assert!(commit(Key::AddKey, "", &c).is_empty());
        assert!(commit(Key::AddKey, "~/.ssh/id_rsa", &c).is_empty());
        let out = commit(Key::AddKey, "~/.ssh/work", &c);
        let list = out[0].args.list("list").unwrap();
        assert_eq!(list.len(), c.default_keys.len() + 1);
    }
}
