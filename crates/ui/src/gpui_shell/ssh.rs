//! SSH в окне GPUI (F03-05, F03 §2.4, §2.5, §2.8): строки «Новое подключение»
//! (`screen-sshnew`), ключа сервера (`screen-sshhostkey`, `screen-sshhostchanged`) и
//! пароля (`screen-sshpass`) внизу панели; группа «SSH» меню дисков
//! (`screen-sshdrives`); раздел «Подключения» настроек (`screen-settings-conn`).
//!
//! Строки — фича окна (ADR-0022): поля ввода (`InputState` gpui-kit) живут здесь, ответы
//! уходят командами `ssh.*`. «Мешочки» (подсказки «Хоста», шаблоны менеджеров паролей)
//! растут вверх от своего поля или кнопки (решение 2026-11-01 «меню от видимых кнопок»).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui_kit::component::Sizable as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    Bounds, ClickEvent, Context, Div, Entity, FocusHandle, Keystroke, Pixels, SharedString,
    Subscription, Window, canvas, div, px,
};
use urfin_core::{AppState, Command, PolicyKey, PolicySet, PolicyValue};
use urfin_i18n::FluentArgs;
use urfin_net::config::PasswordManager;
use urfin_net::conn::PromptKind;
use urfin_net::live::{self, Ask, AskKind, HostHint};
use urfin_net::{Auth, NetState};

use super::CoreHost;
use super::features::{Frame, ShellFeature};
use super::pouch::{H_ITEM, Notch, pouch_up};
use super::screens::Scene;
use super::strip::{Btn, Scope, Tone, button, check, container};
use super::theme::Theme;
use crate::ssh::{SshRow, net};
use crate::tokens::Role;

const ICONS: &str = "Segoe Fluent Icons";
const MONO: &str = "Cascadia Mono";
/// Значки: сервер, предупреждение, щит, ключ, замок, плюс.
const GLYPH_SERVER: &str = "\u{E968}";
const GLYPH_WARN: &str = "\u{E7BA}";
const GLYPH_SHIELD: &str = "\u{EA18}";
const GLYPH_KEY: &str = "\u{E8D7}";
const GLYPH_LOCK: &str = "\u{E72E}";
const GLYPH_ADD: &str = "\u{E710}";
const GLYPH_CHEVRON: &str = "\u{E70D}";

/// Высоты строк (накрывают низ панели, `strip::cover`).
pub(super) const H_FORM: f32 = 166.0;
pub(super) const H_HOSTKEY: f32 = 106.0;
pub(super) const H_CHANGED: f32 = 126.0;
pub(super) const H_SECRET: f32 = 90.0;

/// Поля строки «Новое подключение» и остановки Tab после них.
const HOST: usize = 0;
const USER: usize = 1;
const PORT: usize = 2;
const VIA: usize = 3;
const AUTH: usize = 4;
const SAVE: usize = 5;
const STOPS: usize = 6;

const AUTHS: [Auth; 4] = [Auth::Agent, Auth::Key, Auth::Password, Auth::Command];

/// Шаблоны «Из менеджера паролей» (F03 §2.5): подпись и команда.
fn managers() -> Vec<(String, &'static str)> {
    let i18n = urfin_i18n::current();
    [
        (PasswordManager::BearPass, "BearPass".to_owned()),
        (PasswordManager::Passwork, i18n.get("net-pm-passwork")),
        (PasswordManager::OnePassword, "1Password".to_owned()),
        (PasswordManager::Bitwarden, "Bitwarden".to_owned()),
        (PasswordManager::KeePassXc, "KeePassXC".to_owned()),
    ]
    .into_iter()
    .filter_map(|(m, label)| Some((label, urfin_net::pwcmd::template(m)?)))
    .collect()
}

/// Где в строке нарисовано (прошлый кадр): от этого растут «мешочки».
type Spot = Rc<Cell<Option<Bounds<Pixels>>>>;

fn probe(cell: &Spot) -> impl IntoElement + use<> {
    let cell = Rc::clone(cell);
    canvas(move |b, _, _| cell.set(Some(b)), |_, (), _, _| {})
        .absolute()
        .top_0()
        .left_0()
        .size_full()
}

/// Поле ввода строки и его текст (зеркало — подписка на правку).
struct Field {
    input: Entity<InputState>,
    text: Rc<RefCell<String>>,
    _sub: Subscription,
}

impl Field {
    fn new(
        init: &str,
        masked: bool,
        host: bool,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) -> Field {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(init.to_owned())
                .masked(masked)
        });
        let text = Rc::new(RefCell::new(init.to_owned()));
        let mirror = Rc::clone(&text);
        let sub = cx.subscribe_in(
            &input,
            window,
            move |h: &mut CoreHost, input, ev: &InputEvent, _window, cx| {
                if matches!(ev, InputEvent::Change) {
                    *mirror.borrow_mut() = input.read(cx).value().to_string();
                    // Набор в «Хосте» открывает подсказки (№70).
                    if host && let Some(me) = h.features.get_mut::<Ssh>() {
                        me.hint_open = true;
                        me.hint_hist = false;
                        me.hint_sel = 0;
                    }
                    cx.notify();
                }
            },
        );
        Field {
            input,
            text,
            _sub: sub,
        }
    }

    fn text(&self) -> String {
        self.text.borrow().clone()
    }

    fn focus(&self, window: &mut Window, cx: &mut Context<CoreHost>) {
        self.input.update(cx, |s, cx| s.focus(window, cx));
    }
}

/// Строки SSH: поля, выбор кнопки, флажки, «мешочки».
#[derive(Default)]
#[allow(clippy::struct_excessive_bools, reason = "независимые признаки строк")]
pub(super) struct Ssh {
    /// Номер открытой строки «Новое подключение» (новый — новые поля).
    form_seq: u64,
    fields: Vec<Field>,
    focus: usize,
    auth: Auth,
    save: bool,
    /// Подсказки «Хоста»: открыты, только история (Alt+↓), выбор.
    hint_open: bool,
    hint_hist: bool,
    hint_sel: usize,
    /// Показанный вопрос.
    ask_id: u64,
    secret: Option<Field>,
    /// Имя хоста для замены ключа.
    confirm: Option<Field>,
    replacing: bool,
    /// Кнопка под Tab в строке ключа сервера.
    btn: usize,
    remember: bool,
    /// Шаблоны «Из менеджера паролей» открыты; выбор.
    pouch: bool,
    pouch_sel: usize,
    /// Фокус полю — в следующем кадре (нужно окно).
    want_focus: bool,
    strip_at: Spot,
    host_at: Spot,
    mgr_at: Spot,
}

impl Ssh {
    /// Подсказки «Хоста» для набранного (Alt+↓ — только история хостов).
    fn hints(&self, st: &NetState) -> Vec<HostHint> {
        let text = self.fields.get(HOST).map(Field::text).unwrap_or_default();
        if self.hint_hist {
            return live::host_hints(st, "")
                .into_iter()
                .filter(|h| matches!(h.source, live::HintSource::History(_)))
                .collect();
        }
        if text.trim().is_empty() {
            return Vec::new();
        }
        live::host_hints(st, &text)
            .into_iter()
            .filter(|h| !h.host.eq_ignore_ascii_case(text.trim()) || h.host.len() > text.len())
            .collect()
    }

    fn hints_shown(&self, st: &NetState) -> Option<Vec<HostHint>> {
        let h = self.hints(st);
        (self.hint_open && self.focus == HOST && !h.is_empty()).then_some(h)
    }

    /// «k из N» подсказок для статуса.
    pub(super) fn hint_pos(&self, s: &AppState) -> Option<(usize, usize)> {
        let st = net(s)?;
        st.form.as_ref()?;
        let h = self.hints_shown(st)?;
        Some((self.hint_sel.min(h.len() - 1) + 1, h.len()))
    }

    /// Шаблоны менеджеров открыты (клавиши keybar).
    pub(super) fn pouch_open(&self) -> bool {
        self.pouch
    }

    fn submit(&self, f: &live::Form) -> Command {
        let t = |i: usize| self.fields.get(i).map(Field::text).unwrap_or_default();
        let mut c = Command::new("ssh.connect")
            .arg("host", t(HOST))
            .arg("user", t(USER))
            .arg("port", t(PORT))
            .arg("via", t(VIA))
            .arg("auth", live::auth_id(self.auth))
            .arg("save", self.save)
            .arg("side", crate::side_arg(f.side));
        if let Some(e) = &f.edit {
            c = c.arg("edit", e.clone());
        }
        c
    }

    fn form_key(
        &mut self,
        st: &NetState,
        f: &live::Form,
        ks: &Keystroke,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) -> Option<Vec<Command>> {
        let hints = self.hints_shown(st);
        let m = ks.modifiers;
        let none = Some(Vec::new());
        match ks.key.as_str() {
            "escape" if hints.is_some() => {
                self.hint_open = false;
                none
            }
            "escape" => Some(vec![Command::new("ui.cancel")]),
            "enter" => match hints {
                Some(h) => {
                    let pick = h[self.hint_sel.min(h.len() - 1)].host.clone();
                    if let Some(fl) = self.fields.get(HOST) {
                        fl.text.borrow_mut().clone_from(&pick);
                        fl.input.update(cx, |s, cx| s.set_value(pick, window, cx));
                    }
                    self.hint_open = false;
                    none
                }
                None => Some(vec![self.submit(f)]),
            },
            "down" if m.alt => {
                self.focus = HOST;
                self.hint_open = true;
                self.hint_hist = true;
                self.hint_sel = 0;
                self.want_focus = true;
                none
            }
            "up" | "down" if hints.is_some() => {
                let n = hints.map_or(1, |h| h.len());
                self.hint_sel = if ks.key == "up" {
                    (self.hint_sel + n - 1) % n
                } else {
                    (self.hint_sel + 1) % n
                };
                none
            }
            "tab" => {
                self.focus = if m.shift {
                    (self.focus + STOPS - 1) % STOPS
                } else {
                    (self.focus + 1) % STOPS
                };
                self.hint_open = false;
                if let Some(fl) = self.fields.get(self.focus) {
                    fl.focus(window, cx);
                } else {
                    // Сегменты и флажок: фокус — окну (клавиши разбирает строка).
                    window.blur(cx);
                }
                none
            }
            "left" | "right" if self.focus == AUTH => {
                let i = AUTHS.iter().position(|a| *a == self.auth).unwrap_or(0);
                let n = AUTHS.len();
                self.auth = AUTHS[if ks.key == "left" {
                    (i + n - 1) % n
                } else {
                    (i + 1) % n
                }];
                none
            }
            "space" if self.focus == SAVE => {
                self.save = !self.save;
                none
            }
            // Поле — ему; на сегментах и флажке клавиши дальше не идут.
            _ if self.focus < AUTH => None,
            _ => none,
        }
    }

    #[allow(clippy::too_many_lines, reason = "клавиши трёх вопросов подряд")]
    fn ask_key(
        &mut self,
        a: &Ask,
        ks: &Keystroke,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) -> Option<Vec<Command>> {
        let answer = |choice: &str| Command::new("ssh.answer").arg("choice", choice);
        let none = Some(Vec::new());
        // Ctrl+C — на любой раскладке (по месту клавиши, как сочетания keymap).
        let ctrl_c =
            ks.modifiers.control && super::input::stroke_key(ks) == Some(urfin_keymap::ev::Key::C);
        match &a.kind {
            AskKind::HostKey { fingerprint, .. } => match ks.key.as_str() {
                "tab" => {
                    self.btn = (self.btn + if ks.modifiers.shift { 2 } else { 1 }) % 3;
                    none
                }
                "enter" => Some(vec![answer(["trust", "once", "cancel"][self.btn.min(2)])]),
                "escape" => Some(vec![answer("cancel")]),
                _ if ctrl_c => {
                    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(fingerprint.clone()));
                    none
                }
                _ => none,
            },
            AskKind::HostChanged { fingerprint, .. } => {
                if self.replacing {
                    return match ks.key.as_str() {
                        "enter" => {
                            let host = self.confirm.as_ref().map(Field::text).unwrap_or_default();
                            Some(vec![answer("replace").arg("text", host)])
                        }
                        "escape" => {
                            self.replacing = false;
                            self.confirm = None;
                            self.btn = 0;
                            window.blur(cx);
                            none
                        }
                        _ => None,
                    };
                }
                match ks.key.as_str() {
                    "tab" => {
                        self.btn ^= 1;
                        none
                    }
                    "enter" if self.btn == 1 => {
                        self.replacing = true;
                        self.want_focus = true;
                        none
                    }
                    "enter" | "escape" => Some(vec![answer("cancel")]),
                    _ if ctrl_c => {
                        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                            fingerprint.clone(),
                        ));
                        none
                    }
                    _ => none,
                }
            }
            AskKind::Secret { kind, .. } => {
                if self.pouch {
                    let n = managers().len();
                    return match ks.key.as_str() {
                        "up" => {
                            self.pouch_sel = (self.pouch_sel + n - 1) % n;
                            none
                        }
                        "down" => {
                            self.pouch_sel = (self.pouch_sel + 1) % n;
                            none
                        }
                        "enter" => {
                            let cmd = managers()
                                .get(self.pouch_sel)
                                .map(|(_, c)| (*c).to_owned())
                                .unwrap_or_default();
                            self.pouch = false;
                            Some(vec![answer("cmd").arg("text", cmd)])
                        }
                        "escape" => {
                            self.pouch = false;
                            none
                        }
                        _ => none,
                    };
                }
                match ks.key.as_str() {
                    "down" if ks.modifiers.alt && *kind == PromptKind::Password => {
                        self.pouch = true;
                        self.pouch_sel = 0;
                        none
                    }
                    "enter" => {
                        let text = self.secret.as_ref().map(Field::text).unwrap_or_default();
                        let mut c = answer("ok").arg("text", text);
                        if self.remember {
                            c = c.arg("remember", true);
                        }
                        Some(vec![c])
                    }
                    "escape" => Some(vec![answer("cancel")]),
                    _ => None,
                }
            }
        }
    }
}

/// Строка вопроса или «Новое подключение» накрывает панель: сторона и высота.
pub(super) fn cover(s: &AppState) -> Option<(Scope, f32)> {
    let st = net(s)?;
    if let Some(a) = st.ask() {
        let side = st.live(&a.profile).map_or(s.active, |l| l.side);
        let h = match &a.kind {
            AskKind::HostKey { .. } => H_HOSTKEY,
            AskKind::HostChanged { .. } => H_CHANGED,
            AskKind::Secret { .. } => H_SECRET,
        };
        return Some((Scope::Panel(side), h));
    }
    st.form.as_ref().map(|f| (Scope::Panel(f.side), H_FORM))
}

/// «Запомнить» под политикой: источник замка (`GPO`, `TOML`).
fn remember_lock(s: &AppState) -> Option<&'static str> {
    let e = s.ext.get::<PolicySet>()?.get(PolicyKey::PasswordCache)?;
    matches!(e.value, PolicyValue::Bool(false)).then(|| e.source.label())
}

fn icon(theme: &Theme, glyph: &'static str, role: Role) -> Div {
    div()
        .flex_none()
        .font_family(ICONS)
        .text_size(px(14.0))
        .text_color(theme.c(role))
        .child(SharedString::new_static(glyph))
}

fn label(theme: &Theme, text: String) -> Div {
    div()
        .flex_none()
        .whitespace_nowrap()
        .text_color(theme.c(Role::TextPrimary))
        .child(SharedString::from(text))
}

fn note(theme: &Theme, text: String) -> Div {
    div()
        .flex_none()
        .whitespace_nowrap()
        .text_size(px(12.5))
        .text_color(theme.c(Role::TextSecondary))
        .child(SharedString::from(text))
}

fn mono(theme: &Theme, text: String, role: Role) -> Div {
    div()
        .flex_none()
        .whitespace_nowrap()
        .font_family(MONO)
        .text_size(px(12.0))
        .text_color(theme.c(role))
        .child(SharedString::from(text))
}

/// Поле в рамке шириной `w`; `focused` — рамка `accent`.
fn input_box(theme: &Theme, f: Option<&Field>, w: f32, focused: bool) -> Div {
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
        .bg(theme.c(Role::FillControlInput));
    match f {
        Some(f) => b.child(Input::new(&f.input).small().appearance(false)),
        None => b,
    }
}

impl ShellFeature for Ssh {
    /// Сцена `screen-sshpass`: открыть шаблоны менеджеров (`screens.ssh kind=pouch`).
    fn ext(
        &mut self,
        id: &str,
        _args: &urfin_core::Args,
        _s: &AppState,
        _cx: &mut Context<CoreHost>,
    ) -> Option<Vec<Command>> {
        (id == "screens.ssh.pouch").then(|| {
            self.pouch = true;
            self.pouch_sel = 0;
            Vec::new()
        })
    }

    fn key_in(
        &mut self,
        s: &AppState,
        ks: &Keystroke,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) -> Option<Vec<Command>> {
        let st = net(s)?;
        if let Some(a) = st.ask() {
            return self.ask_key(a, ks, window, cx);
        }
        let f = st.form.clone()?;
        self.form_key(st, &f, ks, window, cx)
    }

    fn editing(&self, s: &AppState) -> bool {
        let Some(st) = net(s) else {
            return false;
        };
        if st.ask().is_some() {
            return self.secret.is_some() || (self.replacing && self.confirm.is_some());
        }
        st.form.is_some() && self.focus < AUTH && !self.fields.is_empty()
    }

    fn input(
        &mut self,
        s: &AppState,
        focus: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) {
        let st = net(s);
        let form = st.and_then(|n| n.form.clone());
        match &form {
            Some(f) if f.seq != self.form_seq => {
                self.form_seq = f.seq;
                self.fields = [&f.host, &f.user, &f.port, &f.via]
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| Field::new(v, false, i == HOST, window, cx))
                    .collect();
                self.auth = f.auth;
                self.save = f.save;
                self.focus = HOST;
                self.hint_open = !f.host.is_empty();
                self.hint_hist = false;
                self.hint_sel = 0;
                self.want_focus = true;
            }
            None if !self.fields.is_empty() => {
                self.fields.clear();
                self.hint_open = false;
                window.focus(focus, cx);
            }
            _ => {}
        }
        match st.and_then(NetState::ask) {
            Some(a) if a.id != self.ask_id => {
                self.ask_id = a.id;
                self.btn = 0;
                self.replacing = false;
                self.confirm = None;
                self.remember = false;
                self.pouch = false;
                self.secret = match &a.kind {
                    AskKind::Secret { echo, .. } => Some(Field::new("", !echo, false, window, cx)),
                    _ => None,
                };
                if let Some(f) = &self.secret {
                    f.focus(window, cx);
                } else {
                    window.focus(focus, cx);
                }
            }
            None if self.ask_id != 0 && (self.secret.is_some() || self.confirm.is_some()) => {
                self.secret = None;
                self.confirm = None;
                self.replacing = false;
                self.ask_id = 0;
                window.focus(focus, cx);
            }
            _ => {}
        }
        if self.replacing && self.confirm.is_none() {
            let f = Field::new("", false, false, window, cx);
            f.focus(window, cx);
            self.confirm = Some(f);
        }
        if self.want_focus {
            self.want_focus = false;
            if let Some(f) = self.fields.get(self.focus) {
                f.focus(window, cx);
            }
        }
    }

    fn strip(&self, f: &Frame<'_>, cx: &Context<CoreHost>) -> Option<Div> {
        let st = net(f.s)?;
        let el = if let Some(a) = st.ask() {
            match &a.kind {
                AskKind::HostKey {
                    algo, fingerprint, ..
                } => self.hostkey_row(a, algo, fingerprint, f, cx),
                AskKind::HostChanged {
                    algo,
                    file,
                    line,
                    old,
                    fingerprint,
                    ..
                } => self.changed_row(a, algo, file, *line, old, fingerprint, f, cx),
                AskKind::Secret { kind, text, .. } => self.secret_row(a, kind, text, f, cx),
            }
        } else {
            let form = st.form.as_ref()?;
            self.form_row(st, form, f, cx)
        };
        Some(el.relative().child(probe(&self.strip_at)))
    }
}

impl Ssh {
    /// Клик → поправить состояние строки (кнопка, флажок, сегмент) без команды Core.
    fn tweak(cx: &Context<CoreHost>, f: impl Fn(&mut Ssh) + 'static) -> super::overlays::Handler {
        Box::new(cx.listener(move |h: &mut CoreHost, _: &ClickEvent, _, cx| {
            if let Some(me) = h.features.get_mut::<Ssh>() {
                f(me);
            }
            cx.notify();
        }))
    }

    /// `screen-sshnew`: заголовок и «Хост»; пользователь, порт, «через»; вход; флажок и
    /// кнопки.
    #[allow(clippy::too_many_lines, reason = "строка сверху вниз")]
    fn form_row(
        &self,
        st: &NetState,
        form: &live::Form,
        f: &Frame<'_>,
        cx: &Context<CoreHost>,
    ) -> Div {
        let i18n = urfin_i18n::current();
        let theme = f.theme;
        let hints = self.hints_shown(st);
        let field = |i: usize, w: f32| {
            let shown = !(i == HOST && hints.is_some());
            input_box(
                theme,
                self.fields.get(i).filter(|_| shown),
                w,
                self.focus == i,
            )
        };
        let host_box = field(HOST, 170.0).relative().child(probe(&self.host_at));
        let row1 = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(icon(theme, GLYPH_SERVER, Role::AccentText))
            .child(
                div()
                    .w(px(140.0))
                    .flex_none()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(SharedString::from(i18n.get(if form.edit.is_some() {
                        "net-form-edit"
                    } else {
                        "net-form-new"
                    }))),
            )
            .child(label(theme, i18n.get("net-field-host")))
            .child(host_box);
        let row2 = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(label(theme, i18n.get("net-field-user")))
            .child(field(USER, 96.0))
            .child(div().w(px(4.0)))
            .child(label(theme, i18n.get("net-field-port")))
            .child(field(PORT, 52.0))
            .child(div().w(px(4.0)))
            .child(label(theme, i18n.get("net-field-via")))
            .child(field(VIA, 130.0));
        let mut seg = div()
            .flex()
            .rounded(px(4.0))
            .border_1()
            .border_color(theme.c(if self.focus == AUTH {
                Role::AccentFill
            } else {
                Role::StrokeControl
            }))
            .overflow_hidden();
        for (k, a) in AUTHS.into_iter().enumerate() {
            let on = a == self.auth;
            seg = seg.child(
                div()
                    .id(SharedString::from(format!("ssh-auth-{k}")))
                    .h(px(28.0))
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .text_size(px(12.5))
                    .when(k > 0, |d| {
                        d.border_l_1().border_color(theme.c(Role::StrokeControl))
                    })
                    .when(on, |d| {
                        d.bg(theme.c(Role::FillCursor))
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                    })
                    .child(SharedString::from(i18n.get(live::auth_key(a))))
                    .on_click(Self::tweak(cx, move |me| me.auth = a)),
            );
        }
        let row3 = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(label(theme, i18n.get("net-field-auth")))
            .child(seg);
        let row4 = div()
            .flex()
            .items_center()
            .gap(px(12.0))
            .child(
                check(
                    theme,
                    "ssh-save".into(),
                    self.save,
                    i18n.get("net-save-profile"),
                    true,
                )
                .when(self.focus == SAVE, |d| {
                    d.rounded(px(3.0))
                        .border_1()
                        .border_color(theme.c(Role::AccentFill))
                })
                .on_click(Self::tweak(cx, |me| me.save = !me.save)),
            )
            .child(
                button(
                    theme,
                    "ssh-connect".into(),
                    i18n.get("net-connect"),
                    "Enter",
                    Btn::Primary,
                )
                .on_click((f.click_all)(vec![self.submit(form)])),
            )
            .child(
                button(
                    theme,
                    "ssh-form-cancel".into(),
                    i18n.get("ui-button-cancel"),
                    "Esc",
                    Btn::Normal,
                )
                .on_click((f.click)(Command::new("ui.cancel"))),
            );
        let mut el = container(theme, Tone::Plain, 12.0, 10.0)
            .gap(px(8.0))
            .child(row1)
            .child(row2)
            .child(row3)
            .child(row4);
        if let Some(h) = hints {
            el = el.child(self.hint_pouch(&h, f));
        }
        el
    }

    /// Подсказки «Хоста» (№70): «мешочек» вверх, корешок — само поле с набранным;
    /// общая часть — `highlight`, источник справа.
    fn hint_pouch(&self, h: &[HostHint], f: &Frame<'_>) -> Div {
        let theme = f.theme;
        let text = self.fields.get(HOST).map(Field::text).unwrap_or_default();
        let fmt = &theme.fmt;
        let now = crate::ssh::now_ns();
        let sel = self.hint_sel.min(h.len().saturating_sub(1));
        let mut body = div().flex().flex_col();
        for (i, it) in h.iter().enumerate() {
            let n = if self.hint_hist {
                0
            } else {
                text.trim().len().min(it.host.len())
            };
            let (head, tail) = it.host.split_at(it.host.floor_char_boundary(n));
            body = body.child(
                div()
                    .h(px(H_ITEM))
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .rounded(px(4.0))
                    .when(i == sel, |d| d.bg(theme.c(Role::FillCursor)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .font_family(MONO)
                            .text_size(px(12.5))
                            .child(
                                div()
                                    .bg(theme.c(Role::Highlight))
                                    .child(SharedString::from(head.to_owned())),
                            )
                            .child(SharedString::from(tail.to_owned())),
                    )
                    .child(note(theme, crate::ssh::hint_source(it, fmt, now))),
            );
        }
        let (strip, host) = (self.strip_at.get(), self.host_at.get());
        let (left, bottom) = match (strip, host) {
            (Some(s), Some(h)) => (
                f32::from(h.origin.x - s.origin.x),
                f32::from((s.origin.y + s.size.height) - (h.origin.y + h.size.height)),
            ),
            _ => (216.0, 118.0),
        };
        let w = 340.0;
        let notch = Notch {
            x: 0.0,
            w: 170.0,
            h: 30.0,
            content: div()
                .w_full()
                .h_full()
                .flex()
                .items_center()
                .children(
                    self.fields
                        .get(HOST)
                        .map(|fl| Input::new(&fl.input).small().appearance(false)),
                )
                .into_any_element(),
        };
        pouch_up(theme, left, bottom, w, Some(notch), body)
    }

    fn hostkey_row(
        &self,
        a: &Ask,
        algo: &str,
        fp: &str,
        f: &Frame<'_>,
        cx: &Context<CoreHost>,
    ) -> Div {
        let i18n = urfin_i18n::current();
        let theme = f.theme;
        let mut args = FluentArgs::new();
        args.set("host", a.plain_host().to_owned());
        let answer = |c: &str| vec![Command::new("ssh.answer").arg("choice", c)];
        let ring = |b: gpui_kit::Stateful<Div>, i: usize| {
            b.when(self.btn == i, |d| d.border_color(theme.c(Role::AccentFill)))
        };
        let _ = cx;
        container(theme, Tone::Waiting, 12.0, 10.0)
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(icon(theme, GLYPH_WARN, Role::ContextRemote))
                    .child(
                        div()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .whitespace_nowrap()
                            .child(SharedString::from(i18n.fmt("net-hostkey-unknown", &args))),
                    )
                    .child(note(theme, i18n.get("net-hostkey-verify"))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(note(theme, algo.to_owned()))
                    .child(mono(theme, fp.to_owned(), Role::TextPrimary)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        ring(
                            button(
                                theme,
                                "ssh-trust".into(),
                                i18n.get("net-trust"),
                                "Enter",
                                Btn::Primary,
                            ),
                            0,
                        )
                        .on_click((f.click_all)(answer("trust"))),
                    )
                    .child(
                        ring(
                            button(
                                theme,
                                "ssh-once".into(),
                                i18n.get("net-once"),
                                "",
                                Btn::Normal,
                            ),
                            1,
                        )
                        .on_click((f.click_all)(answer("once"))),
                    )
                    .child(
                        ring(
                            button(
                                theme,
                                "ssh-hk-cancel".into(),
                                i18n.get("ui-button-cancel"),
                                "Esc",
                                Btn::Normal,
                            ),
                            2,
                        )
                        .on_click((f.click_all)(answer("cancel"))),
                    ),
            )
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "поля вопроса о сменившемся ключе, строка сверху вниз"
    )]
    fn changed_row(
        &self,
        a: &Ask,
        algo: &str,
        file: &std::path::Path,
        line: usize,
        old: &str,
        fp: &str,
        f: &Frame<'_>,
        cx: &Context<CoreHost>,
    ) -> Div {
        let i18n = urfin_i18n::current();
        let theme = f.theme;
        let mut args = FluentArgs::new();
        args.set("host", a.plain_host().to_owned());
        let answer = |c: &str| vec![Command::new("ssh.answer").arg("choice", c)];
        let lbl = |t: String| {
            div()
                .w(px(52.0))
                .flex_none()
                .text_size(px(12.5))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(t))
        };
        let file_name = file
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let actions = if self.replacing {
            let host = self.confirm.as_ref().map(Field::text).unwrap_or_default();
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(label(theme, i18n.get("net-host-name")))
                .child(input_box(theme, self.confirm.as_ref(), 200.0, true))
                .child(
                    button(
                        theme,
                        "ssh-replace-go".into(),
                        i18n.get("net-replace-go"),
                        "Enter",
                        Btn::Danger,
                    )
                    .on_click((f.click_all)(vec![
                        Command::new("ssh.answer")
                            .arg("choice", "replace")
                            .arg("text", host),
                    ])),
                )
                .child(
                    button(
                        theme,
                        "ssh-replace-back".into(),
                        i18n.get("ui-button-cancel"),
                        "Esc",
                        Btn::Normal,
                    )
                    .on_click(Self::tweak(cx, |me| {
                        me.replacing = false;
                        me.confirm = None;
                    })),
                )
        } else {
            let ring = |b: gpui_kit::Stateful<Div>, i: usize| {
                b.when(self.btn == i, |d| d.border_color(theme.c(Role::AccentFill)))
            };
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    ring(
                        button(
                            theme,
                            "ssh-ch-cancel".into(),
                            i18n.get("ui-button-cancel"),
                            "Esc",
                            Btn::Primary,
                        ),
                        0,
                    )
                    .on_click((f.click_all)(answer("cancel"))),
                )
                .child(
                    ring(
                        button(
                            theme,
                            "ssh-replace".into(),
                            i18n.get("net-replace"),
                            "",
                            Btn::Normal,
                        ),
                        1,
                    )
                    .on_click(Self::tweak(cx, |me| {
                        me.replacing = true;
                        me.want_focus = true;
                    })),
                )
                .child(note(theme, i18n.get("net-replace-note")))
        };
        container(theme, Tone::Danger, 12.0, 10.0)
            .gap(px(6.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(icon(theme, GLYPH_SHIELD, Role::Modified))
                    .child(
                        div()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .whitespace_nowrap()
                            .child(SharedString::from(i18n.fmt("net-hostkey-changed", &args))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(lbl(i18n.get("net-was")))
                    .child(note(theme, algo.to_owned()))
                    .child(mono(theme, old.to_owned(), Role::TextPrimary))
                    .child(note(theme, format!("{file_name}:{line}"))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(lbl(i18n.get("net-now")))
                    .child(note(theme, algo.to_owned()))
                    .child(
                        mono(theme, fp.to_owned(), Role::Modified)
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD),
                    ),
            )
            .child(actions)
    }

    #[allow(clippy::too_many_lines, reason = "строка сверху вниз")]
    fn secret_row(
        &self,
        a: &Ask,
        kind: &PromptKind,
        text: &str,
        f: &Frame<'_>,
        cx: &Context<CoreHost>,
    ) -> Div {
        let i18n = urfin_i18n::current();
        let theme = f.theme;
        let mut args = FluentArgs::new();
        args.set("who", a.user_host.clone());
        let (title, password) = match kind {
            PromptKind::Password => (i18n.fmt("net-password-for", &args), true),
            PromptKind::Passphrase(p) => {
                args.set("key", p.display().to_string());
                (i18n.fmt("net-passphrase-for", &args), false)
            }
            PromptKind::Interactive { .. } => (text.trim().to_owned(), false),
        };
        let field = self.secret.as_ref();
        let ok_text = field.map(Field::text).unwrap_or_default();
        let mut row1 = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(icon(theme, GLYPH_KEY, Role::AccentText))
            .child(
                div()
                    .flex_none()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .whitespace_nowrap()
                    .child(SharedString::from(title)),
            )
            .child(input_box(theme, field, 150.0, true));
        if password {
            row1 = row1.child(
                div()
                    .id("ssh-manager")
                    .relative()
                    .flex_none()
                    .h(px(28.0))
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(theme.c(Role::StrokeControl))
                    .bg(theme.c(Role::LayerPanel))
                    .text_size(px(12.5))
                    .when(self.pouch, |d| d.opacity(0.0))
                    .child(SharedString::from(i18n.get("net-from-manager")))
                    .child(icon(theme, GLYPH_CHEVRON, Role::TextSecondary).text_size(px(10.0)))
                    .child(probe(&self.mgr_at))
                    .on_click(Self::tweak(cx, |me| {
                        me.pouch = !me.pouch;
                        me.pouch_sel = 0;
                    })),
            );
        }
        let mut ok = Command::new("ssh.answer")
            .arg("choice", "ok")
            .arg("text", ok_text);
        if self.remember {
            ok = ok.arg("remember", true);
        }
        let mut row2 = div().flex().items_center().gap(px(12.0));
        if password {
            let lock = remember_lock(f.s);
            row2 = row2.child(
                check(
                    theme,
                    "ssh-remember".into(),
                    self.remember && lock.is_none(),
                    i18n.get("net-remember"),
                    lock.is_none(),
                )
                .when(lock.is_none(), |d| {
                    d.on_click(Self::tweak(cx, |me| me.remember = !me.remember))
                }),
            );
            if let Some(src) = lock {
                let mut a = FluentArgs::new();
                a.set("source", src);
                row2 = row2.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .child(icon(theme, GLYPH_LOCK, Role::TextSecondary).text_size(px(11.0)))
                        .child(note(theme, i18n.fmt("net-remember-locked", &a))),
                );
            }
        }
        row2 = row2
            .child(
                button(
                    theme,
                    "ssh-pass-ok".into(),
                    i18n.get("net-connect"),
                    "Enter",
                    Btn::Primary,
                )
                .on_click((f.click_all)(vec![ok])),
            )
            .child(
                button(
                    theme,
                    "ssh-pass-cancel".into(),
                    i18n.get("ui-button-cancel"),
                    "Esc",
                    Btn::Normal,
                )
                .on_click((f.click_all)(vec![
                    Command::new("ssh.answer").arg("choice", "cancel"),
                ])),
            );
        let mut el = container(theme, Tone::Plain, 12.0, 10.0)
            .gap(px(8.0))
            .child(row1)
            .child(row2);
        if self.pouch && password {
            el = el.child(self.manager_pouch(f));
        }
        el
    }

    /// «Из менеджера паролей ▾»: шаблоны команд, корешок — кнопка.
    fn manager_pouch(&self, f: &Frame<'_>) -> Div {
        let i18n = urfin_i18n::current();
        let theme = f.theme;
        let mut body = div().flex().flex_col();
        for (i, (name, cmd)) in managers().into_iter().enumerate() {
            body = body.child(
                div()
                    .id(SharedString::from(format!("ssh-pm-{i}")))
                    .h(px(H_ITEM))
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .rounded(px(4.0))
                    .when(i == self.pouch_sel, |d| d.bg(theme.c(Role::FillCursor)))
                    .child(
                        div()
                            .w(px(92.0))
                            .flex_none()
                            .when(i == self.pouch_sel, |d| {
                                d.font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            })
                            .child(SharedString::from(name)),
                    )
                    .child(mono(theme, cmd.to_owned(), Role::TextSecondary))
                    .on_click((f.click_all)(vec![
                        Command::new("ssh.answer")
                            .arg("choice", "cmd")
                            .arg("text", cmd),
                    ])),
            );
        }
        body = body.child(
            div()
                .mt(px(4.0))
                .pt(px(6.0))
                .px(px(8.0))
                .pb(px(2.0))
                .border_t_1()
                .border_color(theme.c(Role::StrokeDivider))
                .text_size(px(11.5))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(i18n.get("net-pm-note"))),
        );
        let w = 470.0;
        let (strip, btn) = (self.strip_at.get(), self.mgr_at.get());
        let (bx, bw, bottom) = match (strip, btn) {
            (Some(s), Some(b)) => (
                f32::from(b.origin.x - s.origin.x),
                f32::from(b.size.width),
                f32::from((s.origin.y + s.size.height) - (b.origin.y + b.size.height)),
            ),
            _ => (420.0, 180.0, 50.0),
        };
        let left = (bx + bw - w).max(0.0);
        let notch = Notch {
            x: bx - left,
            w: bw,
            h: 30.0,
            content: div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .text_size(px(12.5))
                .child(SharedString::from(i18n.get("net-from-manager")))
                .child(icon(theme, GLYPH_CHEVRON, Role::TextSecondary).text_size(px(10.0)))
                .into_any_element(),
        };
        pouch_up(theme, left, bottom, w, Some(notch), body)
    }
}

/// Профиль в группе «SSH» меню дисков: значок сервера (точка — подключён), имя,
/// `user@host`, метка источника, «когда».
pub(super) fn drive_row(r: &SshRow, on: bool, theme: &Theme) -> Div {
    let fg = |role: Role| theme.c(if on { Role::CursorText } else { role });
    let glyph = div()
        .relative()
        .flex_none()
        .font_family(ICONS)
        .text_size(px(16.0))
        .text_color(fg(Role::AccentText))
        .child(SharedString::new_static(GLYPH_SERVER))
        .when(r.connected, |d| {
            d.child(
                div()
                    .absolute()
                    .right(px(-3.0))
                    .bottom(px(-1.0))
                    .size(px(8.0))
                    .rounded_full()
                    .border_2()
                    .border_color(theme.c(if on {
                        Role::FillCursor
                    } else {
                        Role::LayerPanel
                    }))
                    .bg(theme.c(Role::TypeExec)),
            )
        });
    div()
        .min_h(px(40.0))
        .flex()
        .items_center()
        .gap(px(12.0))
        .px(px(10.0))
        .py(px(4.0))
        .rounded(px(4.0))
        .when(on, |e| e.bg(theme.c(Role::FillCursor)))
        .when(!on, |e| e.hover(|e| e.bg(theme.c(Role::FillSubtleHover))))
        .child(glyph)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .flex_none()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(fg(Role::TextPrimary))
                        .child(SharedString::from(r.profile.clone())),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(px(12.0))
                        .text_color(fg(Role::TextSecondary))
                        .child(SharedString::from(r.user_host.clone())),
                )
                .children(r.tag.map(|t| {
                    div()
                        .flex_none()
                        .px(px(6.0))
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(theme.c(if on {
                            Role::AccentFill
                        } else {
                            Role::StrokeDivider
                        }))
                        .text_size(px(11.0))
                        .text_color(fg(Role::TextSecondary))
                        .child(SharedString::new_static(t))
                })),
        )
        .child(
            div()
                .flex_none()
                .whitespace_nowrap()
                .text_size(px(12.0))
                .text_color(fg(Role::TextSecondary))
                .child(SharedString::from(r.when.clone())),
        )
}

/// «+ Новое подключение… Ins».
pub(super) fn new_row(on: bool, theme: &Theme) -> Div {
    let i18n = urfin_i18n::current();
    let fg = |role: Role| theme.c(if on { Role::CursorText } else { role });
    div()
        .min_h(px(36.0))
        .flex()
        .items_center()
        .gap(px(12.0))
        .px(px(10.0))
        .rounded(px(4.0))
        .when(on, |e| e.bg(theme.c(Role::FillCursor)))
        .when(!on, |e| e.hover(|e| e.bg(theme.c(Role::FillSubtleHover))))
        .child(
            div()
                .flex_none()
                .w(px(16.0))
                .font_family(ICONS)
                .text_size(px(14.0))
                .text_color(fg(Role::AccentText))
                .child(SharedString::new_static(GLYPH_ADD)),
        )
        .child(
            div()
                .flex_1()
                .text_color(fg(Role::TextPrimary))
                .child(SharedString::from(i18n.get("cmd-ssh-new"))),
        )
        .child(
            div()
                .flex_none()
                .text_size(px(12.0))
                .text_color(fg(Role::TextSecondary))
                .child(SharedString::new_static("Ins")),
        )
}

/// Колонки таблицы «Подключений», px: источник, имя, хост, пользователь, вход, через,
/// последнее.
const W: [f32; 7] = [76.0, 224.0, 182.0, 112.0, 142.0, 122.0, 120.0];

/// Раздел «Подключения» (`screen-settings-conn`): поиск и кнопки; таблица профилей
/// (источник — первая колонка, метки одной ширины); «Свойства» выбранного.
#[allow(clippy::too_many_lines, reason = "раздел сверху вниз")]
pub(super) fn settings_conn(
    s: &AppState,
    theme: &Theme,
    click: super::overlays::ClickAll<'_>,
    filter: &str,
    sel: usize,
) -> Div {
    let i18n = urfin_i18n::current();
    let rows = crate::ssh::conn_rows(s, filter);
    let sel = sel.min(rows.len().saturating_sub(1));
    let side = crate::side_arg(s.active);
    let cur = rows.get(sel).map(|p| p.name.clone());
    let btn = |id: &str, label: &str, key: &str, cmds: Vec<Command>| {
        button(
            theme,
            format!("conn-{id}"),
            i18n.get(label),
            key,
            Btn::Normal,
        )
        .on_click(click(cmds))
    };
    let search = div()
        .w(px(260.0))
        .h(px(30.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .rounded(px(4.0))
        .border_1()
        .border_b_2()
        .border_color(theme.c(Role::StrokeControl))
        .bg(theme.c(Role::FillControlInput))
        .text_size(px(13.0))
        .child(if filter.is_empty() {
            div()
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(i18n.get("net-conn-search")))
        } else {
            div().child(SharedString::from(filter.to_owned()))
        });
    let edit_cmds = cur.as_ref().map_or_else(Vec::new, |p| {
        vec![
            Command::new("ui.settings").arg("close", true),
            Command::new("ssh.edit")
                .arg("profile", p.clone())
                .arg("side", side),
        ]
    });
    let remove = cur.as_ref().map_or_else(Vec::new, |p| {
        vec![Command::new("ssh.remove").arg("profile", p.clone())]
    });
    let bar = div()
        .flex()
        .items_center()
        .gap(px(10.0))
        .child(search)
        .child(div().w(px(6.0)))
        .child(btn(
            "add",
            "net-conn-add",
            "Ins",
            vec![
                Command::new("ui.settings").arg("close", true),
                Command::new("ssh.new").arg("side", side),
            ],
        ))
        .child(btn("edit", "net-conn-edit", "F4", edit_cmds))
        .child(btn("remove", "net-conn-remove", "Del", remove))
        .child(btn(
            "import",
            "net-conn-import",
            "",
            vec![Command::new("ssh.reload")],
        ));
    let cell = |w: f32, d: Div| d.w(px(w)).flex_none().min_w_0().overflow_hidden();
    let head_keys = [
        "net-col-source",
        "net-col-name",
        "net-col-host",
        "net-col-user",
        "net-col-auth",
        "net-col-via",
        "net-col-last",
    ];
    let mut table = div()
        .flex()
        .flex_col()
        .rounded(px(6.0))
        .border_1()
        .border_color(theme.c(Role::StrokeDivider))
        .overflow_hidden()
        .child(
            head_keys.iter().zip(W).fold(
                div()
                    .h(px(28.0))
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .bg(theme.c(Role::Header))
                    .border_b_1()
                    .border_color(theme.c(Role::StrokeDivider))
                    .text_size(px(11.5))
                    .text_color(theme.c(Role::TextSecondary)),
                |d, (k, w)| d.child(cell(w, div().child(SharedString::from(i18n.get(k))))),
            ),
        );
    let now = crate::ssh::now_ns();
    for (i, p) in rows.iter().enumerate() {
        let on = i == sel;
        let tag = crate::ssh::source_tag(p).map(|t| {
            div()
                .w(px(56.0))
                .h(px(18.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(3.0))
                .border_1()
                .border_color(theme.c(Role::StrokeControl))
                .bg(theme.c(Role::FillControl))
                .text_size(px(11.0))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::new_static(t))
        });
        let auth = match (p.auth, p.password_cmd.is_empty()) {
            (Auth::Command, false) => {
                let name = managers()
                    .into_iter()
                    .find(|(_, c)| *c == p.password_cmd)
                    .map(|(n, _)| n);
                match name {
                    Some(n) => format!("{} · {n}", i18n.get("net-auth-password")),
                    None => crate::ssh::auth_label(p),
                }
            }
            _ => crate::ssh::auth_label(p),
        };
        let host = if p.port == urfin_net::profile::DEFAULT_PORT {
            p.host.clone()
        } else {
            format!("{}:{}", p.host, p.port)
        };
        let dash = |v: &str| {
            if v.is_empty() {
                "—".to_owned()
            } else {
                v.to_owned()
            }
        };
        let when = crate::ssh::when(
            &theme.fmt,
            p.last_used.as_ref().and_then(live::unix),
            now,
            crate::ssh::When::Table,
        );
        let mut open = vec![
            Command::new("ui.settings").arg("close", true),
            Command::new("sftp.open")
                .arg("profile", p.name.clone())
                .arg("side", side),
        ];
        if !on {
            // Первый клик выбирает строку.
            open = vec![
                Command::new("ui.settings")
                    .arg("section", "conn")
                    .arg("sel", i64::try_from(i).unwrap_or(0)),
            ];
        }
        table = table.child(
            div()
                .id(SharedString::from(format!("conn-row-{i}")))
                .h(px(28.0))
                .px(px(12.0))
                .flex()
                .items_center()
                .border_b_1()
                .border_color(theme.c(Role::StrokeDivider))
                .when(on, |d| {
                    d.border_1()
                        .border_color(theme.c(Role::AccentFill))
                        .rounded(px(2.0))
                })
                .child(cell(W[0], div().children(tag)))
                .child(cell(W[1], div().child(SharedString::from(p.name.clone()))))
                .child(cell(
                    W[2],
                    div()
                        .font_family(MONO)
                        .text_size(px(12.5))
                        .child(SharedString::from(host)),
                ))
                .child(cell(W[3], div().child(SharedString::from(dash(&p.user)))))
                .child(cell(W[4], div().child(SharedString::from(auth))))
                .child(cell(W[5], div().child(SharedString::from(dash(&p.via)))))
                .child(cell(
                    W[6],
                    div()
                        .text_color(theme.c(Role::TextSecondary))
                        .child(SharedString::from(when)),
                ))
                .on_click(click(open)),
        );
    }
    let props = rows.get(sel).map(|p| properties(s, p, theme));
    div()
        .flex()
        .flex_col()
        .gap(px(14.0))
        .pt(px(4.0))
        .child(bar)
        .child(table)
        .children(props)
}

/// «Свойства» профиля: ключ, ключ сервера, начальная папка, закладки, подписи импорта.
fn properties(s: &AppState, p: &urfin_net::Profile, theme: &Theme) -> Div {
    let i18n = urfin_i18n::current();
    let row = |label: String, value: String, note: String| {
        div()
            .min_h(px(36.0))
            .flex()
            .items_center()
            .border_b_1()
            .border_color(theme.c(Role::StrokeDivider))
            .child(
                div()
                    .w(px(264.0))
                    .flex_none()
                    .child(SharedString::from(label)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .font_family(MONO)
                    .text_size(px(12.5))
                    .child(SharedString::from(value)),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(12.0))
                    .text_color(theme.c(Role::TextSecondary))
                    .child(SharedString::from(note)),
            )
    };
    let dash = |v: String| if v.is_empty() { "—".to_owned() } else { v };
    let key_note = if p.key.is_empty() {
        String::new()
    } else {
        i18n.get("net-prop-key-note")
    };
    let server = net(s)
        .and_then(|n| n.host_keys.get(&p.name.to_lowercase()))
        .map_or_else(
            || ("—".to_owned(), i18n.get("net-prop-hostkey-none")),
            |(algo, fp)| {
                (
                    format!("{} {}", algo_short(algo), fp_short(fp)),
                    i18n.get("net-prop-hostkey-known"),
                )
            },
        );
    let notes: Vec<String> = p.notes.iter().map(|n| n.text(&i18n)).collect();
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .pt(px(6.0))
                .pb(px(4.0))
                .text_size(px(15.0))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(SharedString::from(i18n.get("net-props"))),
        )
        .child(row(i18n.get("net-prop-key"), dash(p.key.clone()), key_note))
        .child(row(i18n.get("net-prop-hostkey"), server.0, server.1))
        .child(row(
            i18n.get("net-prop-start"),
            dash(p.start_dir.clone()),
            i18n.get("net-prop-start-note"),
        ))
        .child(row(
            i18n.get("net-prop-bookmarks"),
            dash(p.bookmarks.join(" · ")),
            i18n.get("net-prop-bookmarks-note"),
        ))
        .children(
            (!notes.is_empty())
                .then(|| row(i18n.get("net-prop-notes"), notes.join(" · "), String::new())),
        )
}

/// `ssh-ed25519` → `ED25519`.
fn algo_short(a: &str) -> String {
    a.trim_start_matches("ssh-")
        .trim_start_matches("ecdsa-sha2-")
        .to_uppercase()
}

/// `SHA256:q7Kc9mR2…Lw4f3Ua`.
fn fp_short(fp: &str) -> String {
    let Some(b) = fp.strip_prefix("SHA256:") else {
        return fp.to_owned();
    };
    if b.chars().count() <= 20 {
        return fp.to_owned();
    }
    let head: String = b.chars().take(8).collect();
    let tail: String = b
        .chars()
        .rev()
        .take(7)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("SHA256:{head}…{tail}")
}

/// Сцены SSH (`docs/design/screens/screen-ssh*.png`): фикстура `screens.ssh` — профили,
/// история, `known_hosts`, вопросы; сети и диска нет.
pub(super) const SCENES: &[Scene] = &[
    Scene {
        name: "sshdrives-open",
        steps: &[
            "ui.cancel",
            "resize 1280x760",
            "panel.focus side=left",
            "screens.ssh kind=profiles",
            "ui.drives side=left",
        ],
    },
    // Выбор — на профиле: статус и клавиши профиля.
    Scene {
        name: "screen-sshdrives",
        steps: &["screens.ssh kind=pick"],
    },
    Scene {
        name: "screen-sshnew",
        steps: &["ui.cancel", "ssh.new host=srv1 side=left"],
    },
    Scene {
        name: "screen-sshhostkey",
        steps: &["ui.cancel", "screens.ssh kind=hostkey"],
    },
    Scene {
        name: "screen-sshhostchanged",
        steps: &["ssh.answer choice=cancel", "screens.ssh kind=changed"],
    },
    Scene {
        name: "sshpass-plain",
        steps: &["ssh.answer choice=cancel", "screens.ssh kind=password"],
    },
    // Открыт «Из менеджера паролей ▾» — шаблоны команд.
    Scene {
        name: "screen-sshpass",
        steps: &["screens.ssh kind=pouch"],
    },
    Scene {
        name: "screen-settings-conn",
        steps: &[
            "ssh.answer choice=cancel",
            "screens.ssh stop=true",
            "screens.ssh kind=conn",
            "ui.settings section=conn sel=1",
        ],
    },
    Scene {
        name: "settings-conn-closed",
        steps: &[
            "ui.settings close=true",
            "screens.ssh kind=clear",
            "resize 1200x760",
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_key_labels() {
        assert_eq!(algo_short("ssh-ed25519"), "ED25519");
        assert_eq!(algo_short("ecdsa-sha2-nistp256"), "NISTP256");
        assert_eq!(
            fp_short("SHA256:q7Kc9mR2abcdefghijklmnopqrsLw4f3Ua"),
            "SHA256:q7Kc9mR2…Lw4f3Ua"
        );
        assert_eq!(fp_short("SHA256:short"), "SHA256:short");
    }

    #[test]
    fn manager_templates() {
        let m = managers();
        assert_eq!(m.len(), 5);
        assert!(m[0].1.starts_with("bearpass"));
        assert!(m.iter().all(|(_, c)| !c.is_empty()));
    }
}
