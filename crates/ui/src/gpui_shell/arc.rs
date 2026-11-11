//! Строка распаковки (S8-29, Shift+F2, `screen-extract`, `screen-extractpw`; S8 §3.5,
//! §3.7) как фича окна (ADR-0022). Черновик приходит событием `arc.draft` из handler
//! архивов (S8-28): `stage = "open"` открывает строку — черновик `arc.extract` с полем
//! «куда» (история путей, Alt+↓), `wrong` — ошибка пароля, `close` — закрыть.
//!
//! Подтверждение — `arc.extract confirmed=true dest subdir conflict paths [queue=bg]
//! [password]`; строку закрывает ответ handler'а, не она сама. Esc — `arc.extract
//! cancel=true` и закрыть. Пароль — только в аргументе `password`: поле с маской вне
//! истории полей, сессии и логов.

use gpui_kit::component::Sizable as _;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    ClickEvent, Context, Div, Entity, FocusHandle, Focusable as _, SharedString, Window, div, px,
};
use urfin_core::{AppState, Args, Command};
use urfin_i18n::FluentArgs;

use super::CoreHost;
use super::features::{Frame, ShellFeature};
use super::field::{CONFLICT, Field, Key, StripOpts};
use super::overlays::Handler;
use super::screens::Scene;
use crate::UiState;
use crate::tokens::Role;

/// Команда черновика строки.
pub(super) const DRAFT: &str = "arc.extract";

/// Ряд пароля: поле 28 и зазор 10.
pub(super) const H_PW: f32 = 38.0;

/// Ширина поля пароля (`screen-extractpw`).
const W_PW: f32 = 178.0;

/// Черновик из `arc.draft { stage: "open" }`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Info {
    /// Файл первого архива.
    pub name: String,
    /// Архивов (больше одного — «Распаковать N архивов»).
    pub count: u64,
    /// Выделено внутри архива (0 — весь архив).
    pub items: u64,
    pub size: u64,
    /// Размер — файлов сжатого tar, распакованный не читали.
    pub estimate: bool,
    /// Подпапка первого архива.
    pub subdir: String,
    pub dest: String,
    pub encrypted: bool,
    pub names_encrypted: bool,
    pub cipher: String,
    /// Неверный пароль: осталось попыток.
    pub wrong: Option<i64>,
}

impl Info {
    fn from_args(a: &Args) -> Info {
        let n = |k: &str| a.int(k).and_then(|v| u64::try_from(v).ok()).unwrap_or(0);
        let s = |k: &str| a.str(k).unwrap_or_default().to_owned();
        Info {
            name: s("name"),
            count: n("count").max(1),
            items: n("items"),
            size: n("size"),
            estimate: a.bool("estimate").unwrap_or(false),
            subdir: s("subdir"),
            dest: s("dest"),
            encrypted: a.bool("encrypted").unwrap_or(false),
            names_encrypted: a.bool("names_encrypted").unwrap_or(false),
            cipher: s("cipher"),
            wrong: None,
        }
    }

    /// «Распаковать `backup_1019.7z` · 1,4 ГБ в»: архивов несколько, выделено внутри
    /// архива или весь архив.
    pub(super) fn title(&self) -> String {
        let i18n = urfin_i18n::current();
        let mut size = urfin_i18n::fmt::bytes_with(&i18n, self.size);
        if self.estimate {
            size.insert_str(0, "≈ ");
        }
        let mut a = FluentArgs::new();
        a.set("name", self.name.clone());
        a.set("size", size);
        if self.count > 1 {
            a.set("count", self.count);
            i18n.fmt("arc-extract-title-many", &a)
        } else if self.items > 0 {
            a.set("count", self.items);
            i18n.fmt("arc-extract-title-items", &a)
        } else {
            i18n.fmt("arc-extract-title", &a)
        }
    }

    /// Подпись флажка подпапки.
    pub(super) fn subdir_label(&self) -> String {
        let i18n = urfin_i18n::current();
        if self.count > 1 {
            return i18n.get("arc-extract-subdir-each");
        }
        let mut a = FluentArgs::new();
        a.set("name", self.subdir.clone());
        i18n.fmt("arc-extract-subdir", &a)
    }

    /// Шифр у поля пароля: «AES-256 · имена зашифрованы» или только шифр.
    pub(super) fn cipher_note(&self) -> String {
        if !self.names_encrypted {
            return self.cipher.clone();
        }
        let mut a = FluentArgs::new();
        a.set("method", self.cipher.clone());
        urfin_i18n::current().fmt("arc-pw-names-encrypted", &a)
    }
}

/// Подтверждение строки: `arc.extract confirmed=true …` (контракт S8-28).
pub(super) fn confirm(
    dest: &str,
    subdir: bool,
    conflict: usize,
    paths: bool,
    bg: bool,
    password: Option<&str>,
) -> Command {
    let mut c = Command::new(DRAFT)
        .arg("confirmed", true)
        .arg("dest", dest.trim())
        .arg("subdir", subdir)
        .arg("conflict", CONFLICT[conflict % CONFLICT.len()].0)
        .arg("paths", paths);
    if bg {
        c = c.arg("queue", "bg");
    }
    if let Some(p) = password {
        c = c.arg("password", p);
    }
    c
}

/// Esc и «Отмена»: черновик забыт, строка закрыта.
pub(super) fn cancel() -> Vec<Command> {
    vec![
        Command::new(DRAFT).arg("cancel", true),
        Command::new("ui.cancel"),
    ]
}

/// Открыт черновик распаковки.
pub(super) fn is_open(s: &AppState) -> bool {
    s.ext
        .get::<UiState>()
        .and_then(|u| u.draft.as_ref())
        .is_some_and(|d| d.cmd == DRAFT)
}

/// Строка распаковки: черновик, флажки, поле пароля.
#[allow(clippy::struct_excessive_bools, reason = "флажки строки — независимые")]
pub(super) struct ArcStrip {
    info: Option<Info>,
    /// Черновик уже был в состоянии: его закрытие — конец строки.
    seen: bool,
    subdir: bool,
    paths: bool,
    pw: Option<Entity<InputState>>,
    /// Фокус и выделение — полю пароля в ближайшем кадре.
    want_focus: bool,
    /// Сцена `screen-extractpw`: текст поля пароля.
    demo_pw: Option<String>,
}

impl Default for ArcStrip {
    fn default() -> ArcStrip {
        ArcStrip {
            info: None,
            seen: false,
            subdir: true,
            paths: true,
            pw: None,
            want_focus: false,
            demo_pw: None,
        }
    }
}

impl ArcStrip {
    /// Ряд пароля виден (высота строки).
    pub(super) fn has_pw(&self) -> bool {
        self.info.as_ref().is_some_and(|i| i.encrypted)
    }

    /// Команды подтверждения: поле «куда» и «Если файл есть» — хоста, остальное — здесь.
    fn commit(&self, dest: &str, conflict: usize, bg: bool, cx: &Context<CoreHost>) -> Command {
        // Текст поля — только на время сборки команды; дальше живёт в handler'е
        // (`Zeroizing`).
        let pw = self
            .pw
            .as_ref()
            .filter(|_| self.has_pw())
            .map(|p| p.read(cx).value());
        confirm(
            dest,
            self.subdir,
            conflict,
            self.paths,
            bg,
            pw.as_ref().map(AsRef::as_ref),
        )
    }

    /// Tab: поле «куда» ↔ поле пароля. `false` — второго поля нет.
    fn next_field(
        &self,
        dest: Option<&Entity<InputState>>,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) -> bool {
        let (Some(pw), Some(dest)) = (self.pw.as_ref().filter(|_| self.has_pw()), dest) else {
            return false;
        };
        let to = if pw.read(cx).focus_handle(cx).is_focused(window) {
            dest
        } else {
            pw
        };
        to.update(cx, |s, cx| {
            s.focus(window, cx);
            s.select_all(window, cx);
        });
        true
    }

    /// Переключатель строки по клику (флажки).
    fn tweak(cx: &Context<CoreHost>, f: fn(&mut ArcStrip)) -> Handler {
        Box::new(cx.listener(move |h: &mut CoreHost, _: &ClickEvent, _, cx| {
            if let Some(me) = h.features.get_mut::<ArcStrip>() {
                f(me);
            }
            cx.notify();
        }))
    }

    /// Кнопка подтверждения: команды собираются по клику (пароль не держится в кадре).
    fn go(cx: &Context<CoreHost>, bg: bool) -> Handler {
        Box::new(
            cx.listener(move |h: &mut CoreHost, _: &ClickEvent, window, cx| {
                for c in h.arc_commit(bg, cx) {
                    h.run(c, window, cx);
                }
            }),
        )
    }

    /// Строка (`screen-extract`): заголовок и путь; с паролем — ряд пароля; флажки,
    /// «Если файл есть» и кнопки.
    #[allow(clippy::too_many_lines, reason = "строка сверху вниз")]
    pub(super) fn bar(
        &self,
        f: &Frame<'_>,
        field: Option<&Field>,
        opts: &StripOpts,
        toggle: &dyn Fn(u8) -> Handler,
        cx: &Context<CoreHost>,
    ) -> Option<Div> {
        use super::strip::{Btn, Tone, button, check, container};
        if !is_open(f.s) {
            return None;
        }
        let info = self.info.as_ref()?;
        let theme = f.theme;
        let i18n = urfin_i18n::current();
        let mut top = div().flex().items_center().gap(px(12.0)).child(
            div()
                .flex_none()
                .whitespace_nowrap()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(SharedString::from(info.title())),
        );
        if let Some(fl) = field.filter(|fl| matches!(fl.key, Key::Draft(_))) {
            top = top.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .relative()
                    .child(Input::new(&fl.input))
                    .child(fl.probe()),
            );
        }
        let note = |t: String, role: Role| {
            div()
                .flex_none()
                .whitespace_nowrap()
                .text_size(px(12.5))
                .text_color(theme.c(role))
                .child(SharedString::from(t))
        };
        let pw_row = self.pw.as_ref().filter(|_| info.encrypted).map(|pw| {
            let wrong = info.wrong.is_some();
            let mut b = div()
                .flex_none()
                .w(px(W_PW))
                .h(px(28.0))
                .flex()
                .items_center()
                .rounded(px(4.0))
                .bg(theme.c(Role::FillControlInput))
                .child(Input::new(pw).small().appearance(false));
            b = if wrong {
                b.border_2().border_color(theme.c(Role::Modified))
            } else {
                b.border_1().border_color(theme.c(Role::StrokeControl))
            };
            let mut row = div()
                .flex()
                .items_center()
                .gap(px(12.0))
                .child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .child(SharedString::from(i18n.get("arc-pw-label"))),
                )
                .child(b);
            let cipher = info.cipher_note();
            if !cipher.is_empty() {
                row = row.child(note(cipher, Role::TextSecondary));
            }
            if let Some(left) = info.wrong {
                let mut a = FluentArgs::new();
                a.set("left", left);
                row = row.child(note(i18n.fmt("arc-pw-wrong", &a), Role::Modified));
            }
            row
        });
        let (_, label) = CONFLICT[opts.conflict % CONFLICT.len()];
        let ifexists = div()
            .id("arc-ifexists")
            .flex()
            .items_center()
            .gap(px(8.0))
            .text_size(px(12.5))
            .child(SharedString::from(i18n.get("ui-strip-ifexists")))
            .child(
                div()
                    .relative()
                    .h(px(26.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(theme.c(if opts.dropdown {
                        Role::AccentFill
                    } else {
                        Role::StrokeControl
                    }))
                    .bg(theme.c(Role::LayerPanel))
                    .children(field.map(Field::probe_drop))
                    .child(SharedString::from(i18n.get(label)))
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(theme.c(Role::TextSecondary))
                            .child(SharedString::new_static("▾")),
                    ),
            )
            .on_click(toggle(0));
        let row = div()
            .flex()
            .items_center()
            .gap(px(20.0))
            .child(
                check(
                    theme,
                    "arc-subdir".into(),
                    self.subdir,
                    info.subdir_label(),
                    true,
                )
                .on_click(Self::tweak(cx, |me| me.subdir = !me.subdir)),
            )
            .child(ifexists)
            .child(
                check(
                    theme,
                    "arc-paths".into(),
                    self.paths,
                    i18n.get("arc-extract-keep-paths"),
                    true,
                )
                .on_click(Self::tweak(cx, |me| me.paths = !me.paths)),
            )
            .child(
                div()
                    .ml(px(8.0))
                    .flex()
                    .gap(px(8.0))
                    .child(
                        button(
                            theme,
                            "arc-go".into(),
                            i18n.get("arc-extract-go"),
                            "Enter",
                            Btn::Primary,
                        )
                        .on_click(Self::go(cx, false)),
                    )
                    .child(
                        button(
                            theme,
                            "arc-bg".into(),
                            i18n.get("ui-strip-background"),
                            "Shift+Enter",
                            Btn::Normal,
                        )
                        .on_click(Self::go(cx, true)),
                    )
                    .child(
                        button(
                            theme,
                            "arc-cancel".into(),
                            i18n.get("ui-button-cancel"),
                            "Esc",
                            Btn::Normal,
                        )
                        .on_click((f.click_all)(cancel())),
                    ),
            );
        Some(
            container(theme, Tone::Plain, 16.0, 12.0)
                .child(top)
                .children(pw_row)
                .child(row),
        )
    }
}

impl ShellFeature for ArcStrip {
    fn ext(
        &mut self,
        id: &str,
        args: &Args,
        s: &AppState,
        _cx: &mut Context<CoreHost>,
    ) -> Option<Vec<Command>> {
        if id == "screens.arc.pw" {
            self.demo_pw = Some(args.str("text").unwrap_or_default().to_owned());
            self.want_focus = true;
            return Some(Vec::new());
        }
        if id != "arc.draft" {
            return None;
        }
        Some(match args.str("stage").unwrap_or_default() {
            "open" => {
                let info = Info::from_args(args);
                let open = Command::new("ui.draft")
                    .arg("command", DRAFT)
                    .arg("arg", "dest")
                    .arg("init", info.dest.as_str());
                *self = ArcStrip {
                    want_focus: info.encrypted,
                    info: Some(info),
                    ..ArcStrip::default()
                };
                vec![open]
            }
            "wrong" => {
                if let Some(i) = self.info.as_mut() {
                    i.wrong = Some(args.int("left").unwrap_or(0));
                    self.want_focus = true;
                }
                Vec::new()
            }
            "close" => {
                self.info = None;
                if is_open(s) {
                    vec![Command::new("ui.cancel")]
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        })
    }

    fn input(
        &mut self,
        s: &AppState,
        _focus: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) {
        let open = is_open(s);
        if open {
            self.seen = true;
        } else if self.seen || self.info.is_none() {
            // Строка закрыта: черновик и поле пароля забыты.
            if let Some(pw) = self.pw.take() {
                pw.update(cx, |p, cx| p.set_value("", window, cx));
            }
            self.info = None;
            self.seen = false;
            self.demo_pw = None;
            return;
        }
        if !open || !self.has_pw() {
            return;
        }
        let pw = self
            .pw
            .get_or_insert_with(|| cx.new(|cx| InputState::new(window, cx).masked(true)))
            .clone();
        if let Some(text) = self.demo_pw.take() {
            pw.update(cx, |p, cx| p.set_value(text, window, cx));
        }
        if std::mem::take(&mut self.want_focus) {
            pw.update(cx, |p, cx| {
                p.focus(window, cx);
                p.select_all(window, cx);
            });
        }
    }
}

impl CoreHost {
    /// Подтверждение строки распаковки (Enter, Shift+Enter, кнопки).
    pub(super) fn arc_commit(&self, bg: bool, cx: &Context<CoreHost>) -> Vec<Command> {
        let dest = self
            .field
            .as_ref()
            .filter(|f| matches!(f.key, Key::Draft(_)))
            .map(|f| f.input.read(cx).value().to_string())
            .unwrap_or_default();
        self.features
            .get::<ArcStrip>()
            .map(|a| a.commit(&dest, self.strip_opts.conflict, bg, cx))
            .into_iter()
            .collect()
    }

    /// Tab в строке распаковки: следующее поле. `false` — не её клавиша.
    pub(super) fn arc_tab(&mut self, window: &mut Window, cx: &mut Context<CoreHost>) -> bool {
        let dest = self.field.as_ref().map(|f| f.input.clone());
        self.features
            .get::<ArcStrip>()
            .is_some_and(|a| a.next_field(dest.as_ref(), window, cx))
    }

    /// Строка распаковки для кадра.
    pub(super) fn arc_bar(
        &self,
        f: &Frame<'_>,
        toggle: &dyn Fn(u8) -> Handler,
        cx: &Context<CoreHost>,
    ) -> Option<Div> {
        self.features
            .get::<ArcStrip>()?
            .bar(f, self.field.as_ref(), &self.strip_opts, toggle, cx)
    }
}

/// Сцены строки распаковки: черновик — событием, как от handler'а (`screens.arc`).
pub(super) const SCENES: &[Scene] = &[
    Scene {
        name: "screen-extract",
        steps: &[
            "ui.cancel",
            "sel.none",
            "screens.arc stage=open name=backup_1019.7z count=1 size=1503238553 \
             subdir=backup_1019 dest=\"C:\\Dev\\urfin\\\"",
        ],
    },
    Scene {
        name: "screen-extractpw",
        steps: &[
            "ui.cancel",
            "screens.arc stage=open name=backup_1019.7z count=1 size=1503238553 \
             subdir=backup_1019 dest=\"C:\\Dev\\urfin\\\" encrypted=true \
             names_encrypted=true cipher=AES-256 left=3",
            "screens.arc stage=wrong left=2 pw=backup1019",
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::{Info, cancel, confirm};
    use urfin_core::Args;

    fn ru() {
        let _ = urfin_i18n::set_language("ru");
    }

    #[test]
    fn arc_strip_confirm_args() {
        let c = confirm("  C:\\Dev\\urfin\\ ", true, 3, false, true, Some("секрет"));
        assert_eq!(c.id.as_str(), "arc.extract");
        let a = &c.args;
        assert_eq!(a.bool("confirmed"), Some(true));
        assert_eq!(a.str("dest"), Some("C:\\Dev\\urfin\\"));
        assert_eq!(a.bool("subdir"), Some(true));
        assert_eq!(a.str("conflict"), Some("newer"));
        assert_eq!(a.bool("paths"), Some(false));
        assert_eq!(a.str("queue"), Some("bg"));
        assert_eq!(a.str("password"), Some("секрет"));
        let c = confirm("D:\\", false, 0, true, false, None);
        assert_eq!(c.args.str("conflict"), Some("ask"));
        assert_eq!(c.args.str("queue"), None);
        assert_eq!(c.args.str("password"), None);
        let esc = cancel();
        assert_eq!(esc[0].args.bool("cancel"), Some(true));
        assert_eq!(esc[1].id.as_str(), "ui.cancel");
    }

    #[test]
    fn arc_strip_titles() {
        ru();
        let a = Args::default()
            .with("stage", "open")
            .with("name", "backup_1019.7z")
            .with("count", 1_i64)
            .with("size", 1_503_238_553_i64)
            .with("subdir", "backup_1019")
            .with("encrypted", true)
            .with("names_encrypted", true)
            .with("cipher", "AES-256");
        let i = Info::from_args(&a);
        assert_eq!(i.title(), "Распаковать backup_1019.7z · 1,4 ГБ в");
        assert_eq!(i.subdir_label(), "В подпапку backup_1019");
        assert_eq!(i.cipher_note(), "AES-256 · имена зашифрованы");
        let many = Info {
            count: 3,
            ..i.clone()
        };
        assert!(many.title().starts_with("Распаковать 3 архива · "));
        assert_eq!(many.subdir_label(), "Каждый в свою подпапку");
        let items = Info {
            items: 2,
            names_encrypted: false,
            ..i
        };
        assert!(
            items
                .title()
                .starts_with("Распаковать 2 объекта из backup_1019.7z")
        );
        assert_eq!(items.cipher_note(), "AES-256");
    }
}
