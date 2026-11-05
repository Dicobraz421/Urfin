//! Markdown в редакторе (S7-24, S7 §5.8, §5.9, `screen-editor-md.png`): меню заголовков от
//! кнопки «H ▾», состояние кнопок по строке курсора и предпросмотр рядом — тело делится
//! чертой, справа тот же документ, что во вьювере ([`super::doc`], `Spot::Preview`).
//!
//! Текст и разбор — у handler `editor` (`urfin_view::edit::preview`); здесь только ширины,
//! черта и контекст кнопок.

use gpui_kit::prelude::*;
use gpui_kit::{MouseButton, MouseDownEvent, SharedString, Stateful, div, px};
use urfin_core::Command;
use urfin_view::edit::MdCtx;
use urfin_view::{DocId, EditorDoc, InboxMsg};

use super::super::theme::Theme;
use super::doc::DocView;
use super::editor::{Editor, Item};
use crate::tokens::Role;

/// Меньше каждая часть тела не бывает.
const MIN_W: f32 = 280.0;
/// Полоса прокрутки текста (с границей) и черта между частями.
const W_BAR: f32 = 15.0;
const W_LINE: f32 = 1.0;
/// Захват черты мышью с каждой стороны.
const GRIP: f32 = 3.0;
/// Шаг доли при записи в конфиг.
const FRAC_STEP: f32 = 0.001;

/// Строка Markdown-документа: предпросмотр и кнопки панели.
#[derive(Default)]
pub(super) struct Side {
    /// Список блоков предпросмотра.
    pub doc: DocView,
    /// Доля текста, пока тянут черту.
    frac: Option<f32>,
    /// Контекст кнопок по ревизии документа.
    ctx: Option<(DocId, u64, MdCtx)>,
}

/// Ширина текста без полосы при ширине тела `w` и доле `frac`.
pub(super) fn text_w(w: f32, frac: f32) -> f32 {
    let free = (w - W_BAR - W_LINE).max(0.0);
    if free < 2.0 * MIN_W {
        return (free / 2.0).floor();
    }
    (free * frac).clamp(MIN_W, free - MIN_W).round()
}

/// Левая часть целиком: текст и его полоса.
pub(super) fn left_w(w: f32, frac: f32) -> f32 {
    text_w(w, frac) + W_BAR
}

/// Правая часть: всё, что осталось за чертой.
pub(super) fn right_w(w: f32, frac: f32) -> f32 {
    (w - left_w(w, frac) - W_LINE).max(0.0)
}

/// Доля текста по положению мыши на черте.
fn frac_at(x: f32, w: f32) -> f32 {
    let free = (w - W_BAR - W_LINE).max(1.0);
    ((x - W_BAR) / free).clamp(0.0, 1.0)
}

impl Side {
    /// Контекст кнопок: разбор строки курсора — только когда документ изменился.
    pub fn ctx(&mut self, d: &EditorDoc) -> MdCtx {
        match self.ctx {
            Some((id, rev, c)) if id == d.id && rev == d.rev => c,
            _ => {
                let c = d
                    .engine
                    .as_deref()
                    .and_then(urfin_view::edit::Engine::md_ctx)
                    .unwrap_or_default();
                self.ctx = Some((d.id, d.rev, c));
                c
            }
        }
    }

    /// Доля текста: своя, пока тянут черту, иначе — из конфига.
    pub fn frac(&self, cfg: f64) -> f32 {
        #[allow(clippy::cast_possible_truncation, reason = "доля")]
        self.frac.unwrap_or(cfg as f32)
    }

    /// Мышь тянет черту: `true` — перерисовать.
    pub fn on_drag(&mut self, x: f32, win_w: f32) -> bool {
        if self.frac.is_none() {
            return false;
        }
        self.frac = Some(frac_at(x, win_w));
        true
    }

    /// Кнопка отпущена: новая доля — в конфиг.
    pub fn on_up(&mut self) -> Option<Command> {
        let f = self.frac.take()?;
        let v = (f / FRAC_STEP).round() * FRAC_STEP;
        Some(
            Command::new("ui.setting")
                .arg("key", "editor.preview_split")
                .arg("value", format!("{v:.3}")),
        )
    }
}

/// Черта между текстом и предпросмотром: 1 `border`, захват шире.
pub(super) fn line(
    theme: &Theme,
    cx: &gpui_kit::Context<super::super::CoreHost>,
) -> Stateful<gpui_kit::Div> {
    div()
        .id("editor-split")
        .relative()
        .w(px(W_LINE))
        .flex_none()
        .h_full()
        .bg(theme.c(Role::StrokeDivider))
        .child(
            div()
                .id("editor-split-grip")
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(-GRIP))
                .w(px(2.0 * GRIP + W_LINE))
                .cursor_col_resize()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|h, ev: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        if let Some(e) = h.features.get_mut::<Editor>() {
                            let w = f32::from(window.viewport_size().width);
                            e.md.frac = Some(frac_at(f32::from(ev.position.x), w));
                            cx.notify();
                        }
                    }),
                ),
        )
}

/// Меню заголовков: «Обычный текст», H1–H3; текущий — ✓.
pub(super) fn heading_items(ctx: MdCtx) -> Vec<Item> {
    let i18n = urfin_i18n::current();
    (0..=3u8)
        .map(|n| {
            let label = if n == 0 {
                i18n.get("ui-edit-tb-heading-0")
            } else {
                let mut a = urfin_i18n::FluentArgs::new();
                a.set("level", n);
                i18n.fmt("ui-edit-tb-heading-n", &a)
            };
            Item {
                label: SharedString::from(label),
                hint: String::new(),
                cmd: Command::new("editor.md.heading").arg("level", i64::from(n)),
                divider: n == 1,
                current: ctx.heading == n,
            }
        })
        .collect()
}

/// Подпись кнопки заголовка: «H», на заголовке — его уровень («H2»).
pub(super) fn heading_label(ctx: MdCtx) -> SharedString {
    if ctx.heading == 0 {
        SharedString::new_static("H")
    } else {
        SharedString::from(format!("H{}", ctx.heading))
    }
}

/// Документ сцены `screen-editor-md`: правки как на макете — жирный в строке 35, новая
/// строка 36, курсор в слове «Markdown», верх экрана — строка 12.
pub(super) fn demo_edits(d: &EditorDoc) -> Vec<InboxMsg> {
    let Some(e) = d.engine.as_deref() else {
        return Vec::new();
    };
    let Some(bytes) = e.bytes(0..e.len()) else {
        return Vec::new();
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Vec::new();
    };
    let starts: Vec<u64> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i as u64 + 1))
        .collect();
    let (Some(&top), Some(&l35), Some(&l36)) = (starts.get(11), starts.get(34), starts.get(35))
    else {
        return Vec::new();
    };
    let Some(end) = text[l35 as usize..]
        .find(" — ")
        .map(|k| l35 + k as u64)
    else {
        return Vec::new();
    };
    let doc = d.id;
    let caret = |off: u64| InboxMsg::Caret {
        doc,
        off,
        extend: false,
        unit: urfin_view::inbox::Unit::Char,
        block: None,
    };
    let input = |t: &str| InboxMsg::Input {
        doc,
        text: t.into(),
    };
    let start = l35 + "- [ ] ".len() as u64;
    let word = start + 2 + "Markdown".len() as u64;
    vec![
        caret(l36),
        input("- [ ] Панель инструментов редактора\n"),
        caret(end),
        input("**"),
        caret(start),
        input("**"),
        caret(word),
        InboxMsg::ScrollTo { doc, off: top },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_keeps_both_sides() {
        // Макет: окно 1280 — текст 632, полоса 15, черта 1, документ 632.
        assert!((text_w(1280.0, 0.5) - 632.0).abs() < 1.0);
        assert!((right_w(1280.0, 0.5) - 632.0).abs() < 1.0);
        assert!((text_w(1280.0, 0.01) - MIN_W).abs() < f32::EPSILON);
        assert!((right_w(1280.0, 0.99) - MIN_W).abs() < 1.0);
        // Уже двух минимумов — пополам.
        assert!((text_w(500.0, 0.9) - 242.0).abs() < 1.0);
    }

    #[test]
    fn drag_round_trip() {
        let w = 1280.0;
        let x = left_w(w, 0.3);
        assert!((frac_at(x, w) - 0.3).abs() < 0.01);
    }
}
