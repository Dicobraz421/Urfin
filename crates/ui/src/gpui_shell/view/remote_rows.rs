//! Правка файла с сервера (F03 §4.3, `screen-sftpedit`): чип сервера у таба, строка
//! «Файл с srv1 · сохранение закачает его обратно» и строка «Закачка app.conf → srv1» с
//! полосой и итогом (готово · размер · время, «ждёт связи», ошибка с «Повторить»).

use gpui_kit::prelude::*;
use gpui_kit::{Div, SharedString, div, px, relative};
use urfin_core::Command;
use urfin_view::edit::input::Unit;
use urfin_view::edit::remote::{Remote, UpState, Upload};
use urfin_view::{DocId, EditorDoc, InboxMsg};

use super::super::jobs::icon_button;
use super::super::overlays::Click;
use super::super::strip::{Btn, button};
use super::super::theme::Theme;
use crate::tokens::Role;

const ICON_FONT: &str = "Segoe Fluent Icons";

/// Чип сервера у имени таба (макет: 18, рамка, фон `header`, 11 `secondary`).
pub(super) fn tab_chip(theme: &Theme, label: String) -> Div {
    div()
        .flex_none()
        .h(px(18.0))
        .px(px(6.0))
        .flex()
        .items_center()
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.c(Role::StrokeDivider))
        .bg(theme.c(Role::Header))
        .text_size(px(11.0))
        .text_color(theme.c(Role::TextSecondary))
        .child(SharedString::from(label))
}

/// Строка-пояснение над статусом: «☁ Файл с srv1 · сохранение закачает его обратно».
pub(super) fn note_row(r: &Remote, theme: &Theme) -> Div {
    let i18n = urfin_i18n::current();
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("server", r.label());
    div()
        .h(px(28.0))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(8.0))
        .px(px(12.0))
        .border_t_1()
        .border_color(theme.c(Role::StrokeDivider))
        .bg(theme.c(Role::LayerPanel))
        .text_size(px(12.5))
        .child(
            div()
                .font_family(ICON_FONT)
                .text_size(px(13.0))
                .text_color(theme.c(Role::AccentText))
                .child(SharedString::new_static("\u{E753}")),
        )
        .child(
            div()
                .flex()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(SharedString::from(i18n.fmt("ui-edit-remote-from", &a)))
                .child(
                    div()
                        .text_color(theme.c(Role::TextSecondary))
                        .child(SharedString::from(format!(
                            "\u{a0}·\u{a0}{}",
                            i18n.get("ui-edit-remote-hint")
                        ))),
                ),
        )
}

/// Строку «Закачка» видно: идёт, ждёт связи, ошибка; «готово» — пока нет новых правок.
pub(super) fn upload_shown(d: &EditorDoc) -> Option<&Upload> {
    let u = d.remote.as_deref()?.up.as_ref()?;
    (u.state != UpState::Done || !d.dirty).then_some(u)
}

/// Секунды с десятыми в разделителе языка: «0,2».
fn secs(u: &Upload) -> String {
    let tenths = (u.elapsed.as_millis() + 50) / 100;
    let lang = urfin_i18n::current().lang().clone();
    let dec = urfin_i18n::fmt::NumFmt::of(&lang).decimal;
    format!("{}{dec}{}", tenths / 10, tenths % 10)
}

/// Строка «Закачка app.conf → srv1»: как строка job-бара — подпись, что куда, полоса,
/// итог справа, × (остановить или убрать).
pub(super) fn upload_row(
    d: &EditorDoc,
    u: &Upload,
    id: DocId,
    theme: &Theme,
    click: Click<'_>,
) -> Div {
    let i18n = urfin_i18n::current();
    let server = d
        .remote
        .as_deref()
        .map(Remote::label)
        .unwrap_or_default();
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("name", d.name());
    a.set("server", server);
    let (fill, tone) = match &u.state {
        UpState::Running => (Role::AccentFill, Role::TextSecondary),
        UpState::Waiting => (Role::ContextRemote, Role::ContextRemote),
        UpState::Done => (Role::TypeExec, Role::TypeExec),
        UpState::Failed(_) => (Role::Modified, Role::Modified),
    };
    let mut status = div()
        .w(px(250.0))
        .flex_none()
        .flex()
        .justify_end()
        .items_center()
        .gap(px(6.0))
        .whitespace_nowrap()
        .overflow_hidden()
        .text_color(theme.c(tone));
    let text = match &u.state {
        UpState::Running => format!("{} %", super::rows::pct(u.done, u.total.max(1))),
        UpState::Waiting => i18n.get("ui-edit-upload-waiting"),
        UpState::Done => {
            status = status.child(
                div()
                    .font_family(ICON_FONT)
                    .text_size(px(11.0))
                    .child(SharedString::new_static("\u{E73E}")),
            );
            let mut b = urfin_i18n::FluentArgs::new();
            b.set("size", urfin_i18n::fmt::bytes(u.done));
            b.set("secs", secs(u));
            i18n.fmt("ui-edit-upload-done", &b)
        }
        UpState::Failed(e) => {
            let mut b = urfin_i18n::FluentArgs::new();
            b.set("error", e.clone());
            i18n.fmt("ui-edit-upload-error", &b)
        }
    };
    status = status.child(
        div()
            .min_w_0()
            .overflow_hidden()
            .text_ellipsis()
            .child(SharedString::from(text)),
    );
    let doc = i64::from(id.0);
    let mut row = div()
        .h(px(30.0))
        .flex()
        .items_center()
        .gap(px(12.0))
        .px(px(12.0))
        .text_size(px(12.5))
        .child(
            div()
                .w(px(110.0))
                .flex_none()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(SharedString::from(i18n.get("ui-edit-upload"))),
        )
        .child(
            div()
                .w(px(260.0))
                .flex_none()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(i18n.fmt("ui-edit-upload-what", &a))),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(60.0))
                .h(px(4.0))
                .rounded(px(2.0))
                .overflow_hidden()
                .bg(theme.c(Role::StrokeDivider))
                .child(div().h_full().w(relative(u.frac())).bg(theme.c(fill))),
        )
        .child(status);
    if matches!(u.state, UpState::Failed(_)) {
        row = row.child(
            button(
                theme,
                "edit-upload-retry".to_owned(),
                i18n.get("ui-edit-upload-retry"),
                "",
                Btn::Normal,
            )
            .h(px(24.0))
            .px(px(10.0))
            .on_click(click(Command::new("editor.upload").arg("doc", doc))),
        );
    }
    row = row.child(
        icon_button(theme, "edit-upload-stop".to_owned(), "\u{E711}").on_click(click(
            Command::new("editor.upload")
                .arg("stop", true)
                .arg("doc", doc),
        )),
    );
    div()
        .flex_none()
        .flex()
        .flex_col()
        .py(px(2.0))
        .bg(theme.c(Role::Header))
        .border_t_1()
        .border_color(theme.c(Role::StrokeDivider))
        .child(row)
}

/// Правка сцены `screen-sftpedit` (`screens.edit kind=sftp`): в строке 23 `10m` →
/// `25m`, курсор — в конец строки (как на макете). Через API документа, как ввод.
pub(super) fn demo_edits(d: &EditorDoc) -> Vec<InboxMsg> {
    let Some(e) = d.engine.as_deref() else {
        return Vec::new();
    };
    let Some(bytes) = e.bytes(0..e.len()) else {
        return Vec::new();
    };
    let mut starts = vec![0u64];
    starts.extend(
        bytes
            .iter()
            .enumerate()
            .filter(|(_, b)| **b == b'\n')
            .map(|(i, _)| i as u64 + 1),
    );
    let (Some(&a), Some(&b)) = (starts.get(22), starts.get(23)) else {
        return Vec::new();
    };
    let Some(k) = bytes[a as usize..b as usize]
        .windows(3)
        .position(|w| w == b"10m")
    else {
        return Vec::new();
    };
    let caret = |off: u64, extend: bool| InboxMsg::Caret {
        doc: d.id,
        off,
        extend,
        unit: Unit::Char,
        block: None,
    };
    let at = a + k as u64;
    vec![
        caret(at, false),
        caret(at + 2, true),
        InboxMsg::Input {
            doc: d.id,
            text: "25".into(),
        },
        caret(b - 1, false),
    ]
}
