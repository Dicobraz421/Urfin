//! Экран редактора (S7-16, S7-34; S7 §5.3, §6): фича окна `Editor` (ADR-0022) —
//! `Main::Replace` на месте панелей. Документ — `EditorDoc` на движке `urfin-view`
//! (ADR-0023): текст, курсор, выделение и отмена — у handler `editor`; экран рисует
//! видимые строки своим элементом правки ([`super::edit_view`]) и шлёт ввод
//! (`InboxMsg::Input`, `Caret`, `Scroll`, `ScrollTo`, `Viewport`). Клавиши правки и
//! движения — команды keymap (S7-38).
//!
//! Экран: шапка 36 (путь, «только чтение», счётчик правок; открытие — прогресс), панель
//! инструментов (S7-23, [`super::toolbar`]; Отменить/Вернуть — по журналу документа),
//! тело — полоса изменений, номера, текст, мини-карта 88 ([`super::minimap`]) и полоса
//! прокрутки 14 с метками поиска (всегда, как во вьювере; решение 2026-11-02); внизу —
//! вопрос документа (`EdAsk`) и строка `caution` при `lossy`; статус и keybar. Alt+мышь —
//! блочное выделение (S7-40a), статус «Блок N×M».
//!
//! S7-18, S7-30: строки «Найти» / «Заменить на» (F7, Ctrl+F7) и «Перейти к строке»
//! (Alt+F8) над статусом — [`super::edit_find`]; пока они открыты, фокус Core —
//! `Focus::Overlay(Find)`, а экран — всё ещё редактор.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, ClickEvent, Context, Div, Entity, FocusHandle, Font, Keystroke, MouseButton,
    MouseDownEvent, MouseMoveEvent, SharedString, Window, div, px,
};
use urfin_core::{AppState, Args, Command, Focus, OverlayKind};
use urfin_view::edit::input::Unit;
use urfin_view::index::LineGuess;
use urfin_view::{
    DocId, EdAsk, EdState, EditorDoc, Eol, InboxMsg, Placement, Scroll, ViewState, codepage_menu,
};

use super::super::CoreHost;
use super::super::features::{Frame, Main, ShellFeature};
use super::super::pouch::{H_ITEM, Notch, place, pouch};
use super::super::screens::Scene;
use super::super::strip::{Btn, Tone, button, container};
use super::super::theme::Theme;
use super::edit_bench::{Act, EdBench};
use super::edit_view::{self, EditInput, EditView, ImeRc, Over};
use super::find::{BarAction, BarMode, FindBar, Flag};
use super::rows::{BarPaint, H_HEAD, MONO, Thumb, W_SCROLL, bar_canvas, thumb_frac};
use super::{edit_find, enc_label, head, minimap, toolbar};
use crate::tokens::{Metric, Role};

const ICON_FONT: &str = "Segoe Fluent Icons";
/// Значок «код» шапки (`</>`).
const CODE_ICON: char = '\u{E943}';
/// Пунктов «мешочка» кодировок на виду.
const VISIBLE: usize = 11;
const W_ENC: f32 = 300.0;
const W_EOL: f32 = 160.0;
/// Прогресс «Заменить все» и поиска — перерисовка не чаще.
const PROGRESS: Duration = Duration::from_millis(33);

/// Фича занята (правки сцены, события ещё не ушли, окно текста не отправлено): сцена
/// `--screens` ещё не готова к снимку.
static BUSY: AtomicBool = AtomicBool::new(false);

/// Документ редактора на экране: активный экран — редактор в окне, фокус у него (или у
/// его строки «Найти» / «Перейти»).
pub(super) fn shown(s: &AppState) -> Option<DocId> {
    if !matches!(s.focus, Focus::Editor | Focus::Overlay(OverlayKind::Find)) {
        return None;
    }
    let v = s.ext.get::<ViewState>()?;
    let id = v.active_editor()?;
    (v.editor(id)?.placement == Placement::Window).then_some(id)
}

fn doc(s: &AppState, id: DocId) -> Option<&EditorDoc> {
    s.ext.get::<ViewState>()?.editor(id)
}

/// Сцена `--screens` может сниматься: документ открыт, фоновая работа над видимым
/// закончена, события ушли.
pub(super) fn settled(s: &AppState) -> bool {
    let Some(d) = shown(s).and_then(|id| doc(s, id)) else {
        return true;
    };
    match d.state {
        EdState::Loading { .. } | EdState::Saving(_) | EdState::Replacing => false,
        EdState::Ready => d.idle() && !BUSY.load(Ordering::Relaxed),
        EdState::Failed => true,
    }
}

/// Подпись конца строк: LF, CRLF, CR, «Смешанные».
fn eol_label(e: Eol) -> SharedString {
    SharedString::from(match e {
        Eol::Lf => "LF".to_owned(),
        Eol::CrLf => "CRLF".to_owned(),
        Eol::Cr => "CR".to_owned(),
        Eol::Mixed => urfin_i18n::current().get("ui-view-eol-mixed"),
    })
}

/// Имя языка для статуса (имена грамматик → как пишут люди).
fn lang_name(lang: &str) -> String {
    match lang {
        "toml" => "TOML".into(),
        "json" => "JSON".into(),
        "yaml" => "YAML".into(),
        "xml" => "XML".into(),
        "html" => "HTML".into(),
        "css" => "CSS".into(),
        "ini" => "INI".into(),
        "sql" => "SQL".into(),
        "cmake" => "CMake".into(),
        "javascript" => "JavaScript".into(),
        "typescript" => "TypeScript".into(),
        "powershell" => "PowerShell".into(),
        "csharp" => "C#".into(),
        "cpp" => "C++".into(),
        "c" => "C".into(),
        other => {
            let mut c = other.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect())
                .unwrap_or_default()
        }
    }
}

/// Строк в документе (последний перевод строки не начинает новую): точно или оценкой.
fn line_total(d: &EditorDoc) -> Option<(u64, bool)> {
    let e = d.engine.as_deref()?;
    let len = e.len();
    let g = e.text.line_of(len);
    let unit = u64::from(e.enc.unit().max(1));
    let ends = len >= unit
        && e.bytes(len - unit..len).is_some_and(|t| {
            let b = e.text.sep().byte();
            match t.as_slice() {
                [x] => *x == b,
                [x, y] => (*x == b && *y == 0) || (*x == 0 && *y == b),
                _ => false,
            }
        });
    let n = g.get() + u64::from(!ends || g.get() == 0);
    Some((n.max(1), matches!(g, LineGuess::Exact(_))))
}

/// «41 строка» (оценка — «≈41 строка»).
fn lines_text(d: &EditorDoc) -> String {
    let Some((n, exact)) = line_total(d) else {
        return String::new();
    };
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("count", n);
    let t = urfin_i18n::current().fmt("ui-edit-lines", &a);
    if exact { t } else { format!("≈{t}") }
}

/// Ширина колонки шрифта.
fn advance(font: &Font, size: f32, window: &mut Window) -> f32 {
    let run = gpui_kit::TextRun {
        len: 1,
        font: font.clone(),
        color: gpui_kit::Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let w = window
        .text_system()
        .shape_line(SharedString::new_static("M"), px(size), &[run], None)
        .width;
    f32::from(w).max(1.0)
}

/// Правки сцены `screen-editor` (`screens.edit`): версия 0.4.1 → 0.4.2 (строка 6), две
/// новые строки зависимостей (17–18), курсор — в конец строки 6 (как на макете). Через
/// API документа (`InboxMsg`), как ввод.
fn demo_edits(d: &EditorDoc) -> Vec<InboxMsg> {
    let Some(e) = d.engine.as_deref() else {
        return Vec::new();
    };
    let Some(bytes) = e.bytes(0..e.len()) else {
        return Vec::new();
    };
    let starts: Vec<u64> = std::iter::once(0)
        .chain(
            bytes
                .iter()
                .enumerate()
                .filter(|(_, b)| **b == b'\n')
                .map(|(i, _)| i as u64 + 1),
        )
        .collect();
    let line = |n: usize| starts.get(n).copied();
    let caret = |off: u64, extend: bool| InboxMsg::Caret {
        doc: d.id,
        off,
        extend,
        unit: Unit::Char,
        block: None,
    };
    let input = |t: &str| InboxMsg::Input {
        doc: d.id,
        text: t.into(),
    };
    let mut out = Vec::new();
    if let (Some(a), Some(b)) = (line(5), line(6))
        && let Some(k) = bytes[a as usize..b as usize]
            .windows(5)
            .position(|w| w == b"0.4.1")
    {
        let at = a + k as u64 + 4;
        out.extend([caret(at, false), caret(at + 1, true), input("2")]);
    }
    if let Some(at) = line(16) {
        out.extend([
            caret(at, false),
            input("parking_lot = \"0.12\"\nsmallvec = \"1.13\"\n"),
        ]);
    }
    if let Some(a) = line(5) {
        out.push(caret(a + 17, false));
    }
    out
}

/// Блок сцены `editor-block` (Alt+мышь): от строки 2 колонки 6 (внутри таба) до строки
/// 7 колонки 17 — через табы, кириллицу и CJK.
fn demo_block(d: &EditorDoc) -> Vec<InboxMsg> {
    let Some(e) = d.engine.as_deref() else {
        return Vec::new();
    };
    let Some(bytes) = e.bytes(0..e.len()) else {
        return Vec::new();
    };
    let starts: Vec<u64> = std::iter::once(0)
        .chain(
            bytes
                .iter()
                .enumerate()
                .filter(|(_, b)| **b == b'\n')
                .map(|(i, _)| i as u64 + 1),
        )
        .collect();
    let (Some(&a), Some(&b)) = (starts.get(1), starts.get(6)) else {
        return Vec::new();
    };
    let caret = |off: u64, col: u32, extend: bool| InboxMsg::Caret {
        doc: d.id,
        off,
        extend,
        unit: Unit::Char,
        block: Some(col),
    };
    vec![caret(a, 6, false), caret(b, 17, true)]
}

/// «Мешочек» от кнопки значения панели (или плашка над статусом без панели).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MenuKind {
    Enc,
    Eol,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Menu {
    kind: MenuKind,
    doc: DocId,
    sel: Option<usize>,
    first: usize,
}

/// Фича окна: редактор на месте панелей.
#[derive(Default)]
#[allow(clippy::struct_excessive_bools, reason = "независимые признаки кадра")]
pub(in crate::gpui_shell) struct Editor {
    /// Элемент правки: кэш строк, мышь, размер.
    view: EditView,
    map: minimap::Map,
    /// Ввод текста и IME (общее с сущностью элемента).
    ime: ImeRc,
    input: Option<Entity<EditInput>>,
    /// События handler'у до следующего прохода.
    out: Vec<InboxMsg>,
    /// Отправленное окно текста.
    sent: Option<(DocId, u32, u32)>,
    /// Ширина колонки шрифта прошлого кадра.
    cw: f32,
    /// Фокус у элемента правки (мигание курсора).
    caret_focus: bool,
    /// Последний байт (и колонка блока), отправленный выделением мышью.
    drag_at: Option<(u64, Option<u32>)>,
    /// Тянут блок (Alt+мышь): колонка под мышью — и за концом строки.
    drag_block: bool,
    /// Полоса прокрутки: где бегунок, где схвачен, последнее отправленное смещение.
    thumb: Thumb,
    thumb_grab: Option<f32>,
    thumb_sent: Option<u64>,
    /// Колесо: доли строк и колонок.
    wheel: (f32, f32),
    menu: Option<Menu>,
    /// Документ, которому отдан фокус окна.
    focused: Option<DocId>,
    bench: Option<EdBench>,
    bench_checked: bool,
    /// Окно было активно в прошлом проходе (переход → `InboxMsg::Blur`).
    active: bool,
    /// Правки сцены `screen-editor` ждут готового документа.
    demo: bool,
    /// Блок сцены `editor-block` ждёт готового документа.
    demo_block: bool,
    /// Поиск, замена, переход (S7-18).
    pub(super) find: edit_find::Find,
}

/// Пункт меню.
struct Item {
    label: SharedString,
    hint: String,
    cmd: Command,
    divider: bool,
    current: bool,
}

fn items(kind: MenuKind, d: &EditorDoc) -> Vec<Item> {
    let i18n = urfin_i18n::current();
    match kind {
        MenuKind::Enc => {
            let mut out = vec![Item {
                label: i18n.get("ui-enc-auto").into(),
                hint: i18n.get("ui-enc-auto-hint"),
                cmd: Command::new("editor.codepage").arg("cp", "auto"),
                divider: false,
                current: false,
            }];
            for (group, list) in codepage_menu() {
                for (k, e) in list.iter().enumerate() {
                    out.push(Item {
                        label: enc_label(e.name()),
                        hint: head::enc_hint(group, e.name()),
                        cmd: Command::new("editor.codepage").arg("cp", e.name()),
                        divider: k == 0,
                        current: d.enc.enc.name() == e.name(),
                    });
                }
            }
            out
        }
        MenuKind::Eol => [(Eol::Lf, "lf"), (Eol::CrLf, "crlf"), (Eol::Cr, "cr")]
            .into_iter()
            .map(|(e, arg)| Item {
                label: eol_label(e),
                hint: String::new(),
                cmd: Command::new("editor.eol").arg("to", arg),
                divider: false,
                current: d.eol == e || (d.eol == Eol::Mixed && d.eol_new == e),
            })
            .collect(),
    }
}

/// Шрифт и размер текста (`mono` — моно окна).
fn font_of(v: &ViewState, mono: f32) -> (Font, f32) {
    let family = if v.editor.font.is_empty() {
        SharedString::new_static(MONO)
    } else {
        SharedString::from(v.editor.font.to_string())
    };
    let size = if v.editor.font_size > 0 {
        f32::from(v.editor.font_size)
    } else {
        mono
    };
    (gpui_kit::font(family), size)
}

impl Editor {
    /// Строка документа `id` открыта (фокус Core — у неё).
    fn bar_open(&self, s: &AppState, id: DocId) -> bool {
        self.find
            .bar_for(id.0, s.focus == Focus::Overlay(OverlayKind::Find))
            .is_some()
    }

    /// Фокус окна — у элемента правки.
    fn focus_handle(&self, cx: &gpui_kit::App) -> Option<FocusHandle> {
        self.input.as_ref().map(|e| e.read(cx).focus.clone())
    }

    /// Клавиши «мешочка».
    fn menu_key(&mut self, s: &AppState, ks: &Keystroke) -> Option<Vec<Command>> {
        let m = self.menu?;
        let Some(d) = shown(s).filter(|id| *id == m.doc).and_then(|id| doc(s, id)) else {
            self.menu = None;
            return None;
        };
        let items = items(m.kind, d);
        let n = items.len();
        let mut sel = m
            .sel
            .unwrap_or_else(|| items.iter().position(|i| i.current).unwrap_or(0));
        match ks.key.as_str() {
            "up" => sel = (sel + n - 1) % n,
            "down" => sel = (sel + 1) % n,
            "home" => sel = 0,
            "end" => sel = n.saturating_sub(1),
            "pageup" => sel = sel.saturating_sub(VISIBLE),
            "pagedown" => sel = (sel + VISIBLE).min(n.saturating_sub(1)),
            "enter" => {
                self.menu = None;
                return Some(items.get(sel).map(|i| i.cmd.clone()).into_iter().collect());
            }
            "escape" => {
                self.menu = None;
                return Some(Vec::new());
            }
            _ => return Some(Vec::new()),
        }
        self.menu = Some(Menu {
            sel: Some(sel),
            first: head::keep_visible(m.first, sel, n),
            ..m
        });
        Some(Vec::new())
    }

    /// Клавиши строки вопроса: буква кнопки, Enter — первая, Esc — «Отмена».
    fn ask_key(ask: EdAsk, id: DocId, ks: &Keystroke) -> Vec<Command> {
        let m = &ks.modifiers;
        let answer = |c: &str| {
            vec![
                Command::new("editor.answer")
                    .arg("choice", c)
                    .arg("doc", i64::from(id.0)),
            ]
        };
        let choices = ask.choices();
        match ks.key.as_str() {
            "escape" => answer("cancel"),
            "enter" if !m.alt && !m.control => answer(choices[0].id),
            _ if m.control || m.alt || m.platform => Vec::new(),
            _ => {
                let key = super::super::input::stroke_key(ks);
                choices
                    .iter()
                    .find(|c| {
                        key.is_some_and(|k| {
                            urfin_keymap::ev::Key::from_name(&c.key.to_string()) == Some(k)
                        }) || ks.key.chars().eq(std::iter::once(c.key))
                    })
                    .map(|c| answer(c.id))
                    .unwrap_or_default()
            }
        }
    }

    /// Колесо → прокрутка документа: 3 строки на щелчок, Shift — по горизонтали.
    fn wheel(&mut self, ev: &gpui_kit::ScrollWheelEvent) -> Option<Scroll> {
        let (dx, dy) = match ev.delta {
            gpui_kit::ScrollDelta::Lines(p) => (p.x * 3.0, p.y * 3.0),
            gpui_kit::ScrollDelta::Pixels(p) => (
                f32::from(p.x) / edit_view::ROW_H,
                f32::from(p.y) / edit_view::ROW_H,
            ),
        };
        let (dx, dy) = if ev.modifiers.shift {
            (dy, 0.0)
        } else {
            (dx, dy)
        };
        self.wheel.0 += dx;
        self.wheel.1 += dy;
        let take = |w: &mut f32| {
            let whole = w.trunc();
            *w -= whole;
            whole
        };
        let (x, y) = (take(&mut self.wheel.0), take(&mut self.wheel.1));
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "строк за событие немного"
        )]
        let (xa, ya) = (x.abs() as u32, y.abs() as u32);
        // `+` — вверх.
        match (x, y) {
            (_, y) if y > 0.0 => Some(Scroll::Up(ya)),
            (_, y) if y < 0.0 => Some(Scroll::Down(ya)),
            (x, _) if x > 0.0 => Some(Scroll::Left(xa * 4)),
            (x, _) if x < 0.0 => Some(Scroll::Right(xa * 4)),
            _ => None,
        }
    }

    /// Шаг замера `--bench-editor`: события документа вместо ввода.
    fn bench_step(&mut self, d: &EditorDoc, window: &mut Window) {
        let Some(b) = self.bench.as_mut() else {
            return;
        };
        window.request_animation_frame();
        let Some(e) = d.engine.as_deref() else {
            return;
        };
        if b.waiting() {
            if d.is_ready() && d.idle_text() && self.sent.is_some() {
                b.bytes = e.len();
                b.lines = line_total(d).map_or(0, |x| x.0);
                b.opened();
            }
            return;
        }
        let (busy, fresh_n, last_ms) = d.hl_stats();
        if b.hl(busy, fresh_n, last_ms) {
            return;
        }
        let doc = d.id;
        match b.frame() {
            Act::None => {}
            Act::Down(n) => self.out.push(InboxMsg::Scroll {
                doc,
                scroll: Scroll::Down(n),
            }),
            Act::Type => self.out.push(InboxMsg::Input {
                doc,
                text: "x".into(),
            }),
            Act::Jump(per_mille) => {
                let off =
                    u64::try_from(u128::from(e.len()) * u128::from(per_mille) / 1000).unwrap_or(0);
                self.out.extend([
                    InboxMsg::ScrollTo { doc, off },
                    InboxMsg::Caret {
                        doc,
                        off,
                        extend: false,
                        unit: Unit::Char,
                        block: None,
                    },
                ]);
            }
            Act::Exit => {
                use std::io::Write as _;
                let _ = std::io::stdout().flush();
                std::process::exit(0);
            }
        }
    }
}

/// Клик, двойной, тройной — символ, слово, строка.
fn unit_of(clicks: usize) -> Unit {
    match clicks {
        0 | 1 => Unit::Char,
        2 => Unit::Word,
        _ => Unit::Line,
    }
}

/// Мышь над текстом: клик — курсор, Shift — выделение до точки, двойной — слово,
/// тройной — строка; фокус — элементу правки.
fn mouse_down(h: &mut CoreHost, ev: &MouseDownEvent, w: &mut Window, cx: &mut Context<CoreHost>) {
    let Some(id) = h.core.as_ref().and_then(|c| shown(c.state())) else {
        return;
    };
    let Some(e) = h.features.get_mut::<Editor>() else {
        return;
    };
    let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
    // Поле свёртки (S7-35): ▾/▸ — свернуть или развернуть строку-заголовок.
    let head = e.view.hit.borrow().fold_at(x, y);
    if let Some(head) = head {
        let at = i64::try_from(head).unwrap_or(i64::MAX);
        if let Some(core) = h.core.as_mut() {
            core.dispatch(
                Command::new("editor.fold")
                    .arg("at", at)
                    .from(urfin_core::Origin::Mouse),
            );
        }
        h.pump(w, cx);
        return;
    }
    let Some(off) = e.view.hit.borrow().byte_at(x, y) else {
        return;
    };
    // Alt — блок: колонка под мышью (и за концом строки).
    let block = ev
        .modifiers
        .alt
        .then(|| e.view.hit.borrow().col_at(x))
        .flatten();
    let unit = if block.is_some() {
        Unit::Char
    } else {
        unit_of(ev.click_count)
    };
    e.out.push(InboxMsg::Caret {
        doc: id,
        off,
        extend: ev.modifiers.shift && unit == Unit::Char,
        unit,
        block,
    });
    e.view.drag = unit == Unit::Char;
    e.drag_block = block.is_some();
    e.drag_at = Some((off, block));
    if let Some(f) = e.focus_handle(cx) {
        w.focus(&f, cx);
    }
    h.sync_inbox(w);
    cx.notify();
}

/// Мышь тянут с нажатой кнопкой: выделение до точки; за краем — автопрокрутка.
fn mouse_move(h: &mut CoreHost, ev: &MouseMoveEvent, w: &mut Window, cx: &mut Context<CoreHost>) {
    let id = h.core.as_ref().and_then(|c| shown(c.state()));
    let Some(e) = h.features.get_mut::<Editor>() else {
        return;
    };
    if ev.pressed_button != Some(MouseButton::Left) {
        e.view.drag = false;
        e.view.auto = None;
        e.thumb_grab = None;
        return;
    }
    let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
    // Бегунок полосы прокрутки: верх экрана — на долю документа.
    if let (Some(grab), Some(t), Some(doc)) = (e.thumb_grab, e.thumb.get(), id) {
        let len = h
            .core
            .as_ref()
            .and_then(|c| doc_len(c.state(), doc))
            .unwrap_or(0);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "доля документа"
        )]
        let off = (f64::from(thumb_frac(t, grab, y)) * len as f64) as u64;
        if e.thumb_sent != Some(off) {
            e.thumb_sent = Some(off);
            e.out.push(InboxMsg::ScrollTo { doc, off });
            h.sync_inbox(w);
            cx.notify();
        }
        return;
    }
    if !e.view.drag {
        return;
    }
    let past = e.view.hit.borrow().past(y);
    e.view.auto = past.map(|_| ((x, y), e.view.auto.and_then(|a| a.1)));
    let at = e.view.hit.borrow().byte_at(x, y);
    let block = if e.drag_block {
        e.view.hit.borrow().col_at(x)
    } else {
        None
    };
    if let (Some(doc), Some(off)) = (id, at)
        && e.drag_at != Some((off, block))
    {
        e.drag_at = Some((off, block));
        e.out.push(InboxMsg::Caret {
            doc,
            off,
            extend: true,
            unit: Unit::Char,
            block,
        });
        h.sync_inbox(w);
        cx.notify();
    }
}

/// Длина документа на экране (бегунок полосы прокрутки).
fn doc_len(s: &AppState, id: DocId) -> Option<u64> {
    doc(s, id)?
        .engine
        .as_deref()
        .map(urfin_view::edit::Engine::len)
}

/// Нажатие на полосе прокрутки: выше или ниже бегунка — страница, на бегунке —
/// перетаскивание.
fn bar_down(h: &mut CoreHost, ev: &MouseDownEvent, w: &mut Window, cx: &mut Context<CoreHost>) {
    let Some(id) = h.core.as_ref().and_then(|c| shown(c.state())) else {
        return;
    };
    let Some(e) = h.features.get_mut::<Editor>() else {
        return;
    };
    let Some(t) = e.thumb.get() else {
        return;
    };
    let y = f32::from(ev.position.y);
    let scroll = if y < t.y0 {
        Scroll::PageUp(1)
    } else if y > t.y1 {
        Scroll::PageDown(1)
    } else {
        e.thumb_grab = Some(y - t.y0);
        e.thumb_sent = None;
        return;
    };
    e.out.push(InboxMsg::Scroll { doc: id, scroll });
    h.sync_inbox(w);
    cx.notify();
}

impl ShellFeature for Editor {
    fn ext(
        &mut self,
        id: &str,
        args: &Args,
        s: &AppState,
        cx: &mut Context<CoreHost>,
    ) -> Option<Vec<Command>> {
        match id {
            // Сцены: правки `screen-editor` (S7-16), тексты строк перехода и замены (S7-18).
            "screens.edit" => {
                match args.str("kind") {
                    None => self.demo = true,
                    Some("block") => self.demo_block = true,
                    Some("goto") => self.find.demo = Some(("29:7".to_owned(), None)),
                    Some(_) => {
                        self.find.demo = Some((
                            r#"^(\w+) = "([\d.]+)""#.to_owned(),
                            Some(r#"$1 = { version = "$2" }"#.to_owned()),
                        ));
                    }
                }
                cx.notify();
                Some(Vec::new())
            }
            "editor.search" | "editor.replace" | "editor.goto" => {
                let id_doc = urfin_view::handler::ext_doc(args)?;
                let d = doc(s, id_doc)?;
                self.menu = None;
                let mode = if id == "editor.goto" {
                    BarMode::Goto
                } else {
                    BarMode::Find
                };
                let cmd = self.find.open(d, mode, id == "editor.replace");
                cx.notify();
                Some(cmd.into_iter().collect())
            }
            // У документа поиска не было: прошлый образец строки — снова, иначе строка.
            "editor.search_next" | "editor.search_prev" => {
                let d = urfin_view::handler::ext_doc(args).and_then(|id| doc(s, id))?;
                let back = id == "editor.search_prev";
                let last = self.find.last.clone().or_else(|| edit_find::last_of(d));
                Some(match last {
                    Some((t, f)) => vec![edit_find::search_cmd(d, &t, f, back)],
                    None => vec![Command::new("editor.search")],
                })
            }
            // Строку закрывает смена фокуса (кадр `input`).
            "editor.search_cancel" => {
                cx.notify();
                Some(Vec::new())
            }
            // Перенос и панель — из `[editor]` в кадре.
            "editor.wrap" | "editor.toolbar" | "editor.highlight" => {
                // Панель спрятана — «мешочка» от её кнопки больше нет.
                self.menu = None;
                self.view.clear();
                cx.notify();
                Some(Vec::new())
            }
            "editor.codepage_menu" | "editor.eol_menu" => {
                let doc = urfin_view::handler::ext_doc(args)?;
                self.menu = Some(Menu {
                    kind: if id == "editor.eol_menu" {
                        MenuKind::Eol
                    } else {
                        MenuKind::Enc
                    },
                    doc,
                    sel: None,
                    first: 0,
                });
                cx.notify();
                Some(Vec::new())
            }
            _ => None,
        }
    }

    fn key_in(
        &mut self,
        s: &AppState,
        ks: &Keystroke,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) -> Option<Vec<Command>> {
        if let Some(c) = self.menu_key(s, ks) {
            return Some(c);
        }
        let id = shown(s)?;
        // Строка «Найти» / «Перейти»: свои клавиши, остальное — её полю.
        if self.bar_open(s, id) {
            let bar = self.find.bar.as_mut()?;
            bar.track_focus(window, cx);
            let act = bar.key(ks)?;
            if act == BarAction::Tab {
                bar.focus_other(window, cx);
                cx.notify();
                return Some(Vec::new());
            }
            let d = doc(s, id)?;
            let cmds = self.find.action(d, act);
            cx.notify();
            return Some(cmds);
        }
        let d = doc(s, id)?;
        // Вопрос внизу забирает клавиши; правку и движения разбирает keymap.
        d.ask.map(|ask| Self::ask_key(ask, id, ks))
    }

    fn editing(&self, s: &AppState) -> bool {
        shown(s).is_some_and(|id| {
            self.bar_open(s, id) && self.find.bar.as_ref().is_some_and(FindBar::has_field)
        })
    }

    /// События handler'у: ввод, мышь, окно текста (`Viewport` — по размеру элемента
    /// прошлого кадра), потеря фокуса окном (автосохранение `focus`).
    fn inbox(&mut self, s: &AppState, _theme: &Theme, window: &mut Window) -> bool {
        let active = window.is_window_active();
        if std::mem::replace(&mut self.active, active) && !active && shown(s).is_some() {
            self.out.push(InboxMsg::Blur);
        }
        let Some(v) = s.ext.get::<ViewState>() else {
            self.out.clear();
            return false;
        };
        if let Some(id) = shown(s)
            && let Some((w, h)) = self.view.size.get()
            && self.cw > 0.0
        {
            let (cols, rows) = edit_view::grid(w, h, self.cw, v.editor.gutter);
            if self.sent != Some((id, cols, rows)) {
                self.sent = Some((id, cols, rows));
                self.out.push(InboxMsg::Viewport {
                    doc: id,
                    cols,
                    rows,
                });
            }
        }
        let mut typed = std::mem::take(&mut self.ime.borrow_mut().out);
        self.out.append(&mut typed);
        if self.out.is_empty() {
            return false;
        }
        let tx = v.inbox.sender();
        for m in self.out.drain(..) {
            tx.send(m);
        }
        true
    }

    /// Мигание курсора (при фокусе), шаг автопрокрутки выделения, прогресс задач.
    fn tick(&mut self, s: &AppState, now: Instant) -> Option<Duration> {
        let d = shown(s).and_then(|id| doc(s, id))?;
        let mut next: Option<Duration> = None;
        let mut at = |x: Duration| next = Some(next.map_or(x, |n| n.min(x)));
        if self.caret_focus
            && let Some((_, t)) = self.view.blink_from
        {
            let ms = now.saturating_duration_since(t).as_millis() % edit_view::BLINK.as_millis();
            let left = edit_view::BLINK.as_millis().saturating_sub(ms);
            at(Duration::from_millis(
                u64::try_from(left).unwrap_or(1).max(1),
            ));
        }
        if self.view.drag && self.view.auto.is_some() {
            at(edit_view::AUTO_STEP);
        }
        let busy = d.replace.is_some() || d.search.as_ref().is_some_and(|x| x.st.running);
        if busy {
            at(PROGRESS);
        }
        next
    }

    /// Шаг автопрокрутки: экран — на строки за краем (1…10 за шаг), выделение — до
    /// крайней строки прошлого кадра.
    fn due(&mut self, s: &AppState, now: Instant) -> Vec<Command> {
        let Some(((x, y), last)) = self.view.auto else {
            return Vec::new();
        };
        let Some(doc) = shown(s) else {
            self.view.auto = None;
            return Vec::new();
        };
        if !self.view.drag {
            self.view.auto = None;
            return Vec::new();
        }
        if last.is_some_and(|t| now.saturating_duration_since(t) < edit_view::AUTO_STEP) {
            return Vec::new();
        }
        let Some(past) = self.view.hit.borrow().past(y) else {
            self.view.auto = None;
            return Vec::new();
        };
        self.view.auto = Some(((x, y), Some(now)));
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "1…10 строк"
        )]
        let n = past.abs().ceil().clamp(1.0, 10.0) as u32;
        let scroll = if past < 0.0 {
            Scroll::Up(n)
        } else {
            Scroll::Down(n)
        };
        self.out.push(InboxMsg::Scroll { doc, scroll });
        if let Some(off) = self.view.hit.borrow().byte_at(x, y) {
            let block = if self.drag_block {
                self.view.hit.borrow().col_at(x)
            } else {
                None
            };
            self.out.push(InboxMsg::Caret {
                doc,
                off,
                extend: true,
                unit: Unit::Char,
                block,
            });
        }
        Vec::new()
    }

    fn input(
        &mut self,
        s: &AppState,
        focus: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) {
        if !self.bench_checked {
            self.bench_checked = true;
            if crate::bench::EDITOR.load(Ordering::Relaxed) {
                self.bench = Some(EdBench::new());
            }
        }
        let Some(v) = s.ext.get::<ViewState>() else {
            return;
        };
        if self.menu.is_some_and(|m| v.editor(m.doc).is_none()) {
            self.menu = None;
        }
        if self.input.is_none() {
            let host = cx.entity().downgrade();
            let ime = self.ime.clone();
            self.input = Some(cx.new(|cx| EditInput::new(ime, host, cx)));
        }
        let Some(id) = shown(s) else {
            if self.focused.take().is_some() {
                window.focus(focus, cx);
            }
            self.find.closed();
            self.view.clear();
            self.sent = None;
            BUSY.store(false, Ordering::Relaxed);
            return;
        };
        let Some(d) = v.editor(id) else {
            return;
        };
        let open = self.bar_open(s, id);
        if open {
            if let Some(b) = self.find.bar.as_mut() {
                b.sync(window, cx);
            }
        } else if self.find.bar.is_some() {
            self.find.closed();
            // Фокус — снова тексту.
            self.focused = None;
        }
        self.ime.borrow_mut().update(d);
        if self.demo && d.is_ready() && d.idle() {
            self.demo = false;
            self.out.extend(demo_edits(d));
        }
        if self.demo_block && d.is_ready() && d.idle() {
            self.demo_block = false;
            self.out.extend(demo_block(d));
        }
        // Фокус — тексту: при открытии экрана и когда он вернулся окну (клик мимо); не
        // пока открыта строка поиска (фокус у её поля).
        if !open
            && (self.focused != Some(id) || focus.is_focused(window))
            && let Some(f) = self.focus_handle(cx)
        {
            window.focus(&f, cx);
            self.focused = Some(id);
        }
        self.bench_step(d, window);
        let busy = self.demo
            || self.demo_block
            || !self.out.is_empty()
            || self.sent.is_none_or(|x| x.0 != id)
            || self.view.size.get().is_none();
        BUSY.store(busy, Ordering::Relaxed);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "экран: шапка, панель, тело, строки, статус"
    )]
    fn main(
        &mut self,
        f: &Frame<'_>,
        _hidden: bool,
        window: &mut Window,
        cx: &Context<CoreHost>,
    ) -> Option<Main> {
        let id = shown(f.s)?;
        let v = f.s.ext.get::<ViewState>()?;
        let d = v.editor(id)?;
        if d.ask.is_some() {
            // Вопрос внизу забирает клавиши: меню закрыто.
            self.menu = None;
        }
        let theme = f.theme;
        let narrow = f32::from(window.viewport_size().width) < super::super::chrome::NARROW_W;
        let measure = head::Measure::new(window);
        let e = d.engine.as_deref();
        let live = e.is_some()
            && matches!(
                d.state,
                EdState::Ready | EdState::Saving(_) | EdState::Replacing
            );
        let i18n = urfin_i18n::current();
        let tb = toolbar::Tb {
            dirty: d.dirty || d.is_new,
            undo: e.is_some_and(|e| e.undo.can_undo()),
            redo: e.is_some_and(|e| e.undo.can_redo()),
            wrap: v.editor.wrap,
            readonly: d.readonly,
            enc: enc_label(d.enc.enc.name()),
            eol: eol_label(d.eol),
        };
        let show_tb = v.editor.toolbar && live;
        let mut root = div()
            .id("editor")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(theme.c(Role::LayerPanel))
            .child(header(f.s, d, theme, f.click, &measure));
        if show_tb {
            root = root.child(toolbar::toolbar(&tb, f.s, theme, f.click, &measure));
        }
        // Строка «Найти» / «Перейти» (S7-18) и метки её совпадений.
        let bar = self
            .find
            .bar_for(id.0, f.s.focus == Focus::Overlay(OverlayKind::Find));
        let hits_on = bar.is_some_and(|b| b.mode == BarMode::Find && !b.text().is_empty());
        let marks = d
            .search
            .as_ref()
            .filter(|_| hits_on)
            .map(|x| Arc::clone(&x.st.marks));
        // Тело: элемент правки с мини-картой, пока документ открыт; иначе пусто.
        let body = if live {
            let gutter = v.editor.gutter;
            let (font, size) = font_of(v, theme.m(Metric::TypeMono));
            let cw = advance(&font, size, window);
            self.cw = cw;
            let rows = self.view.size.get().map_or_else(
                || e.map_or(1, |e| e.viewport().1),
                |(w, h)| edit_view::grid(w, h, cw, gutter).1,
            );
            // Окно текста меряется в `paint`: новое — следующим кадром в `Viewport`.
            let want = self
                .view
                .size
                .get()
                .map(|(w, h)| edit_view::grid(w, h, cw, gutter));
            if want.is_none() || want.map(|(c, r)| (id, c, r)) != self.sent {
                window.request_animation_frame();
            }
            let fh = self.input.as_ref().map(|i| i.read(cx).focus.clone());
            let focused =
                window.is_window_active() && fh.as_ref().is_some_and(|f| f.is_focused(window));
            self.caret_focus = focused;
            let over = Over {
                font: &font,
                font_px: size,
                cw,
                rows,
                gutter,
                hits: hits_on,
                focused,
                now: Instant::now(),
            };
            let screen = {
                let ime = self.ime.borrow();
                self.view.build(d, &over, &ime, theme, window)
            };
            let input = fh.zip(self.input.clone());
            let mut row = div().flex_1().min_h_0().relative().flex();
            if let Some(screen) = screen {
                let vis = (screen.from, screen.to);
                let len = e.map_or(0, urfin_view::edit::Engine::len);
                let first = screen.first_line.map(LineGuess::get);
                let count = screen.line_count;
                let text = div()
                    .id("editor-text")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .cursor_text()
                    .child(edit_view::element(
                        screen,
                        theme,
                        &self.view,
                        input,
                        self.ime.clone(),
                    ))
                    .on_mouse_down(MouseButton::Left, cx.listener(mouse_down))
                    .on_scroll_wheel(cx.listener(|h, ev: &gpui_kit::ScrollWheelEvent, w, cx| {
                        let Some(id) = h.core.as_ref().and_then(|c| shown(c.state())) else {
                            return;
                        };
                        let Some(e) = h.features.get_mut::<Editor>() else {
                            return;
                        };
                        if let Some(scroll) = e.wheel(ev) {
                            e.out.push(InboxMsg::Scroll { doc: id, scroll });
                            h.sync_inbox(w);
                            cx.notify();
                        }
                    }));
                row = row.child(text);
                if gutter {
                    let h_map = self.view.size.get().map_or(400.0, |s| s.1);
                    self.map.update(d, first, count, minimap::fit(h_map));
                    row = row.child(
                        div()
                            .id("editor-map")
                            .w(px(minimap::W))
                            .flex_none()
                            .h_full()
                            .border_l_1()
                            .border_color(theme.c(Role::StrokeDivider))
                            .child(minimap::map(&self.map, vis, theme, marks.clone()))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|h, ev: &MouseDownEvent, w, cx| {
                                    let Some(id) = h.core.as_ref().and_then(|c| shown(c.state()))
                                    else {
                                        return;
                                    };
                                    let Some(e) = h.features.get_mut::<Editor>() else {
                                        return;
                                    };
                                    if let Some(off) =
                                        minimap::line_at(&e.map, f32::from(ev.position.y))
                                    {
                                        e.out.push(InboxMsg::ScrollTo { doc: id, off });
                                        h.sync_inbox(w);
                                        cx.notify();
                                    }
                                }),
                            ),
                    );
                }
                // Полоса прокрутки 14 — всегда, справа от мини-карты (решение 2026-11-02):
                // бегунок — видимые байты из длины, метки — совпадения поиска.
                let lenf = len.max(1);
                #[allow(clippy::cast_precision_loss, reason = "доля документа")]
                let span = (
                    vis.0.min(lenf) as f32 / lenf as f32,
                    (vis.1.saturating_sub(vis.0) as f32 / lenf as f32).min(1.0),
                );
                let bar_marks: Arc<[u64]> = marks.unwrap_or_else(|| Arc::from(Vec::new()));
                row = row.child(
                    div()
                        .id("editor-scroll")
                        .w(px(W_SCROLL))
                        .flex_none()
                        .h_full()
                        .border_l_1()
                        .border_color(theme.c(Role::StrokeDivider))
                        .child(bar_canvas(
                            span,
                            bar_marks,
                            len,
                            self.thumb.clone(),
                            BarPaint::of(theme),
                        ))
                        .on_mouse_down(MouseButton::Left, cx.listener(bar_down)),
                );
            }
            row.into_any_element()
        } else {
            div().flex_1().into_any_element()
        };
        root = root.child(body);
        if let Some(ask) = d.ask {
            root = root.child(ask_row(ask, d, id, theme, f.click));
        } else if d.lossy && d.is_ready() {
            root = root.child(lossy_row(d, theme, f.click));
        }
        if let (Some(b), true, None) = (bar, live, d.ask) {
            let editable = d.is_ready() && !d.readonly;
            root = root.child(bar_row(b, d, editable, f, cx));
        }
        let zones = status(d, v, show_tb);
        let keys = if d.ask.is_some() {
            Vec::new()
        } else if let Some(b) = bar {
            super::find::editor_keys(b.mode, b.has_replace())
        } else if self.menu.is_some() {
            vec![
                ("↑ ↓".to_owned(), i18n.get("ui-keys-select")),
                ("Enter".to_owned(), i18n.get("ui-keys-apply")),
                ("Esc".to_owned(), i18n.get("ui-keys-close")),
            ]
        } else {
            keybar_keys()
        };
        root = root
            .child(super::super::chrome::status(&zones, narrow, theme))
            .child(super::super::chrome::keybar(
                &[],
                crate::keybar::Mods::None,
                Some(&keys),
                narrow,
                theme,
                &f.click,
            ))
            // Выделение тянется и за пределами строк.
            .on_mouse_move(cx.listener(mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|h, _: &gpui_kit::MouseUpEvent, _, _| {
                    if let Some(e) = h.features.get_mut::<Editor>() {
                        e.view.drag = false;
                        e.view.auto = None;
                        e.drag_at = None;
                        e.drag_block = false;
                        e.thumb_grab = None;
                    }
                }),
            );
        // Открытие, «Заменить все»: полоса 2 px `accent.fill` под шапкой.
        let progress = match d.state {
            EdState::Loading { done, total } if total > 0 => {
                #[allow(clippy::cast_precision_loss, reason = "доля")]
                let frac = (done as f32 / total as f32).clamp(0.0, 1.0);
                Some(frac)
            }
            _ => d
                .replace
                .as_ref()
                .map(urfin_view::edit::replace::ReplaceJob::progress),
        };
        if let Some(frac) = progress {
            root = root.child(
                div()
                    .absolute()
                    .top(px(H_HEAD - 2.0))
                    .left_0()
                    .h(px(2.0))
                    .w(gpui_kit::relative(frac))
                    .bg(theme.c(Role::AccentFill)),
            );
        }
        if let Some(m) = self.menu.filter(|m| m.doc == id) {
            root = root.children(menu(m, d, &tb, show_tb, theme, &measure, cx));
        }
        Some(Main::Replace(root.into_any_element()))
    }
}

/// Строка «Найти» (+ «Заменить на») или «Перейти к строке» документа `d`.
fn bar_row(
    b: &FindBar,
    d: &EditorDoc,
    editable: bool,
    f: &Frame<'_>,
    cx: &Context<CoreHost>,
) -> Div {
    let i18n = urfin_i18n::current();
    match b.mode {
        BarMode::Find => {
            let toggle = |fl: Flag| -> super::super::overlays::Handler {
                Box::new(cx.listener(move |h, _: &ClickEvent, _, cx| {
                    if let Some(b) = h
                        .features
                        .get_mut::<Editor>()
                        .and_then(|e| e.find.bar.as_mut())
                    {
                        b.flags.toggle(fl);
                    }
                    cx.notify();
                }))
            };
            let counter = if b.text().is_empty() {
                (String::new(), false)
            } else {
                super::counter_of(d.search.as_ref().map(|x| &x.st))
            };
            let replace = |all: bool| -> super::super::overlays::Handler {
                Box::new(cx.listener(move |h, _: &ClickEvent, w, cx| {
                    let cmds = h
                        .features
                        .get_mut::<Editor>()
                        .map(|e| e.find.replace(all))
                        .unwrap_or_default();
                    for c in cmds {
                        h.run(c, w, cx);
                    }
                }))
            };
            let repl = b.replace_row(f.theme, editable, replace(false), replace(true));
            b.find_row(f.theme, counter, &toggle).children(repl)
        }
        BarMode::Goto => {
            let text = b.text();
            let hint = if !text.trim().is_empty() && urfin_view::goto::parse(&text).is_err() {
                (i18n.get("ui-goto-invalid"), true)
            } else {
                let mut a = urfin_i18n::FluentArgs::new();
                let total = match line_total(d) {
                    Some((n, true)) => n.to_string(),
                    Some((n, false)) => format!("≈{n}"),
                    None => String::new(),
                };
                a.set("total", total);
                (i18n.fmt("ui-goto-of-line-col", &a), false)
            };
            let go: super::super::overlays::Handler =
                Box::new(cx.listener(move |h, _: &ClickEvent, w, cx| {
                    let to = h
                        .features
                        .get_mut::<Editor>()
                        .and_then(|e| e.find.bar.as_ref().map(FindBar::text))
                        .filter(|t: &String| urfin_view::goto::parse(t).is_ok());
                    if let Some(t) = to {
                        h.run(Command::new("editor.goto").arg("to", t.trim()), w, cx);
                    }
                }));
            let cancel = (f.click)(Command::new("editor.search_cancel"));
            b.goto_row(f.theme, i18n.get("ui-goto-line"), hint, go, cancel)
        }
    }
}

/// Клавиши keybar редактора (по макету).
fn keybar_keys() -> Vec<(String, String)> {
    let i18n = urfin_i18n::current();
    let k = |key: &str, label: &str| (key.to_owned(), i18n.get(label));
    vec![
        k("F2", "keybar-editor-save"),
        k("F7", "keybar-editor-search"),
        k("Ctrl+F7", "keybar-editor-replace"),
        k("Alt+F8", "keybar-editor-goto"),
        k("Esc", "keybar-editor-close"),
    ]
}

/// Статус: «Стр 6, Кол 18» (до индекса — «Стр ≈N»; колонка за первым сегментом длинной
/// строки — «Кол ≈N») · «41 строка» · «TOML · Пробелы: 4 · Вставка» (без панели — ещё
/// кодировка и конец строки, №61; задача документа — «Замена 41 %»).
fn status(d: &EditorDoc, v: &ViewState, toolbar: bool) -> [String; 3] {
    let i18n = urfin_i18n::current();
    let Some(e) = d.engine.as_deref() else {
        return [String::new(), String::new(), String::new()];
    };
    let line = e.text.line_of(e.sel.caret);
    let col = u64::from(d.cursor.1) + 1;
    let col_exact = e.caret_seg().map_or(true, |s| !s.cont);
    let mut a = urfin_i18n::FluentArgs::new();
    let pos = match line {
        LineGuess::Exact(k) => {
            a.set("line", k + 1);
            if col_exact {
                a.set("col", col);
            } else {
                a.set("col", format!("≈{col}"));
            }
            i18n.fmt("ui-edit-pos", &a)
        }
        LineGuess::Approx(k) => {
            a.set("line", k + 1);
            let mut t = i18n.fmt("ui-edit-line-approx", &a);
            if !col_exact {
                a.set("col", col);
                t.push_str(", ");
                t.push_str(&i18n.fmt("ui-edit-col-approx", &a));
            }
            t
        }
    };
    // Блок: «Блок N×M» — строк × колонок.
    let pos = match e.block_area() {
        Some(b) => {
            let mut a = urfin_i18n::FluentArgs::new();
            let rows = e
                .text
                .line_of(b.bottom)
                .get()
                .saturating_sub(e.text.line_of(b.top).get())
                + 1;
            a.set("rows", rows);
            a.set("cols", u64::from(b.c1 - b.c0));
            format!("{pos} · {}", i18n.fmt("ui-edit-block", &a))
        }
        None => pos,
    };
    let mut right = Vec::new();
    if let Some(j) = &d.replace {
        let mut a = urfin_i18n::FluentArgs::new();
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "проценты"
        )]
        a.set("pct", (j.progress() * 100.0).round() as u64);
        right.push(i18n.fmt("ui-edit-job-replace", &a));
    }
    if let Some(l) = d.lang {
        right.push(lang_name(l));
    }
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("n", u64::from(v.editor.tab_size));
    right.push(i18n.fmt(
        if v.editor.expand_tabs {
            "ui-edit-spaces"
        } else {
            "ui-edit-tabs"
        },
        &a,
    ));
    if !toolbar {
        right.push(enc_label(d.enc.enc.name()).to_string());
        right.push(eol_label(d.eol).to_string());
    }
    right.push(i18n.get(if e.overwrite {
        "ui-edit-overtype"
    } else {
        "ui-edit-insert"
    }));
    [pos, lines_text(d), right.join(" · ")]
}

/// Шапка 36: кнопка «История» (Alt+F11, S7-25), значок, путь к папке, «только чтение»;
/// справа счётчик правок или открытие.
fn header(
    s: &urfin_core::AppState,
    d: &EditorDoc,
    theme: &Theme,
    click: super::super::overlays::Click<'_>,
    _m: &head::Measure,
) -> Div {
    let i18n = urfin_i18n::current();
    let folder = d
        .loc
        .path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let mut row = div()
        .h(px(H_HEAD))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(8.0))
        .px(px(12.0))
        .border_b_1()
        .border_color(theme.c(Role::StrokeDivider))
        .child(super::lists::history_button(s, theme, click))
        .child(
            div()
                .font_family(ICON_FONT)
                .text_size(px(14.0))
                .text_color(theme.c(Role::AccentText))
                .child(SharedString::from(CODE_ICON.to_string())),
        )
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(px(13.0))
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(folder)),
        );
    if d.readonly {
        row = row.child(head::readonly_tone(
            chip(theme, i18n.get("ui-view-readonly")),
            theme,
        ));
    }
    match d.state {
        EdState::Loading { done, total } => {
            let mut a = urfin_i18n::FluentArgs::new();
            a.set("pct", super::rows::pct(done, total));
            row = row.child(div().flex_1()).child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.c(Role::TextSecondary))
                    .child(SharedString::from(i18n.fmt("ui-edit-loading", &a))),
            );
        }
        _ if !d.changes.is_empty() => {
            row = row.child(div().flex_1()).child(changes_chip(d, theme));
        }
        _ => {}
    }
    row
}

/// Части «+2 добавлено · 1 изменена · −1 удалено» с ролью цвета (палитра B — как
/// полосы изменений у номеров строк: `exec`, `accent_text`, `modified`).
fn changes_parts(d: &EditorDoc) -> Vec<(String, Role)> {
    let i18n = urfin_i18n::current();
    let c = &d.changes;
    let mut parts = Vec::new();
    for (n, key, role) in [
        (c.added, "ui-edit-added", Role::TypeExec),
        (c.modified, "ui-edit-changed", Role::AccentKey),
        (c.deleted, "ui-edit-removed", Role::Modified),
    ] {
        if n > 0 {
            let mut a = urfin_i18n::FluentArgs::new();
            a.set("count", u64::from(n));
            parts.push((i18n.fmt(key, &a), role));
        }
    }
    parts
}

/// Чип правок: части цветом ролей (500), разделитель `secondary`, фон `panel`. В высокой
/// контрастности — одним цветом, как был.
fn changes_chip(d: &EditorDoc, theme: &Theme) -> Div {
    let parts = changes_parts(d);
    if !theme.vivid() {
        let text: Vec<String> = parts.into_iter().map(|(t, _)| t).collect();
        return chip(theme, text.join(" · "));
    }
    let mut el = chip(theme, String::new()).bg(theme.c(Role::LayerPanel));
    for (i, (text, role)) in parts.into_iter().enumerate() {
        if i > 0 {
            el = el.child(SharedString::new_static("\u{a0}·\u{a0}"));
        }
        el = el.child(
            div()
                .text_color(theme.c(role))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(SharedString::from(text)),
        );
    }
    el
}

/// Чип шапки: рамка `stroke.control`, текст 12 `secondary`.
fn chip(theme: &Theme, text: String) -> Div {
    div()
        .flex_none()
        .h(px(22.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.c(Role::StrokeControl))
        .text_size(px(12.0))
        .text_color(theme.c(Role::TextSecondary))
        .child(SharedString::from(text))
}

/// Строка вопроса документа (S7 §5.4, ADR-0023 §3, §8): текст и кнопки сразу за ним;
/// первая — основная.
fn ask_row(
    ask: EdAsk,
    d: &EditorDoc,
    id: DocId,
    theme: &Theme,
    click: super::super::overlays::Click<'_>,
) -> Div {
    let i18n = urfin_i18n::current();
    let tone = match ask {
        EdAsk::Close | EdAsk::ReadOnly => Tone::Plain,
        EdAsk::Changed
        | EdAsk::Unmappable { .. }
        | EdAsk::NoChar { .. }
        | EdAsk::Writer
        | EdAsk::NoSpace { .. } => Tone::Waiting,
    };
    let mut row = div().flex().items_center().gap(px(10.0)).child(
        div()
            .flex_none()
            .whitespace_nowrap()
            .child(SharedString::from(ask.text(d))),
    );
    for (k, c) in ask.choices().iter().enumerate() {
        let kind = if k == 0 { Btn::Primary } else { Btn::Normal };
        let key = if c.id == "cancel" {
            "Esc".to_owned()
        } else {
            c.key.to_ascii_uppercase().to_string()
        };
        row = row.child(
            button(
                theme,
                format!("edit-ask-{}", c.id),
                i18n.get(c.label),
                &key,
                kind,
            )
            .on_click(click(
                Command::new("editor.answer")
                    .arg("choice", c.id)
                    .arg("doc", i64::from(id.0)),
            )),
        );
    }
    container(theme, tone, 12.0, 10.0).child(row)
}

/// Строка `caution` о недекодируемых байтах (S7 §5.1, ADR-0023 §3): сохранятся как были.
fn lossy_row(d: &EditorDoc, theme: &Theme, click: super::super::overlays::Click<'_>) -> Div {
    let i18n = urfin_i18n::current();
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("enc", enc_label(d.enc.enc.name()).to_string());
    let row = div()
        .flex()
        .items_center()
        .gap(px(10.0))
        .child(
            div()
                .flex_none()
                .whitespace_nowrap()
                .child(SharedString::from(i18n.fmt("ui-edit-lossy-kept", &a))),
        )
        .child(
            button(
                theme,
                "edit-lossy-enc".to_owned(),
                i18n.get("ui-edit-change-enc"),
                "F8",
                Btn::Normal,
            )
            .on_click(click(Command::new("editor.codepage_menu"))),
        );
    container(theme, Tone::Waiting, 12.0, 10.0).child(row)
}

/// «Мешочек» кодировок или концов строк от кнопки панели; без панели — плашка над
/// статусом справа.
#[allow(clippy::too_many_lines, reason = "пункты, корешок и щит — одна фигура")]
fn menu(
    m: Menu,
    d: &EditorDoc,
    tb: &toolbar::Tb,
    with_toolbar: bool,
    theme: &Theme,
    me: &head::Measure,
    cx: &Context<CoreHost>,
) -> Vec<AnyElement> {
    let items = items(m.kind, d);
    let n = items.len();
    let sel = m
        .sel
        .unwrap_or_else(|| items.iter().position(|i| i.current).unwrap_or(0));
    let first = head::keep_visible(m.first, sel, n);
    let w = match m.kind {
        MenuKind::Enc => W_ENC,
        MenuKind::Eol => W_EOL,
    };
    let mut body = div().flex().flex_col().relative();
    for (i, it) in items.iter().enumerate().skip(first).take(VISIBLE) {
        if it.divider && i != first {
            body = body.child(
                div()
                    .h(px(9.0))
                    .flex()
                    .items_center()
                    .child(div().h(px(1.0)).flex_1().bg(theme.c(Role::StrokeDivider))),
            );
        }
        let cmd = it.cmd.clone();
        let mut row = div()
            .id(SharedString::from(format!("editor-menu-{i}")))
            .h(px(H_ITEM))
            .px(px(8.0))
            .flex()
            .items_center()
            .rounded(px(4.0))
            .text_size(px(13.0))
            .child(
                div()
                    .w(px(22.0))
                    .flex_none()
                    .font_family(ICON_FONT)
                    .text_size(px(10.0))
                    .children(
                        it.current
                            .then(|| SharedString::from(crate::icons::ACCEPT.to_string())),
                    ),
            )
            .child(it.label.clone())
            .child(div().flex_1().min_w(px(16.0)))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.c(Role::TextSecondary))
                    .child(SharedString::from(it.hint.clone())),
            )
            .hover(|s| s.bg(theme.c(Role::FillSubtleHover)))
            .on_click(cx.listener(move |h, _: &ClickEvent, w, cx| {
                if let Some(e) = h.features.get_mut::<Editor>() {
                    e.menu = None;
                }
                h.run(cmd.clone(), w, cx);
            }));
        if i == sel {
            row = row.bg(theme.c(Role::FillCursor));
        }
        body = body.child(row);
    }
    let fig = if with_toolbar {
        let [(x_enc, w_enc), (x_eol, w_eol)] = toolbar::values_x(me, tb, me.win_w);
        let (ax, aw, text) = match m.kind {
            MenuKind::Enc => (x_enc, w_enc, &tb.enc),
            MenuKind::Eol => (x_eol, w_eol, &tb.eol),
        };
        let (left, nx) = place(ax, aw, w, me.win_w, w - aw);
        let top = H_HEAD + (toolbar::H - 28.0) / 2.0;
        let notch = Notch {
            x: nx,
            w: aw,
            h: H_HEAD + toolbar::H - top,
            content: div()
                .flex()
                .flex_col()
                .child(toolbar::value_inner(text, theme))
                .into_any_element(),
        };
        pouch(theme, left, top, w, Some(notch), body)
    } else {
        #[allow(clippy::cast_precision_loss, reason = "пунктов немного")]
        let h = n.min(VISIBLE) as f32 * H_ITEM + 16.0;
        let left = (me.win_w - w - 12.0).max(0.0);
        let top = (me.win_h - 2.0 * 26.0 - h - 8.0).max(H_HEAD);
        pouch(theme, left, top, w, None, body)
    };
    let fig = fig.id("editor-menu").occlude();
    let shield = div()
        .id("editor-menu-shield")
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .occlude()
        .on_click(cx.listener(|h, _: &ClickEvent, _, cx| {
            if let Some(e) = h.features.get_mut::<Editor>() {
                e.menu = None;
            }
            cx.notify();
        }));
    vec![shield.into_any_element(), fig.into_any_element()]
}

/// Сцены редактора (S7-16, S7-23, S7-34): `screen-editor` — Cargo.toml с правками
/// (счётчик, полоса изменений, ●), панель инструментов, курсор на строке 6; без панели —
/// кодировка и EOL в статусе; «мешочек» концов строк; вопрос при закрытии с правками.
pub(in crate::gpui_shell) const SCENES: &[Scene] = &[
    // S7-40a: блок Alt+мышью через табы, кириллицу и CJK (todo.txt демо-дерева).
    Scene {
        name: "editor-block",
        steps: &[
            "ui.cancel",
            "resize 1280x760",
            "panel.focus side=left",
            "editor.open path=\"Projects\\postman\\todo.txt\"",
            "screens.edit kind=block",
        ],
    },
    Scene {
        name: "screen-editor",
        steps: &[
            "editor.close",
            "ui.cancel",
            "resize 1280x760",
            "panel.focus side=left",
            "editor.open path=\"Projects\\urfin\\Cargo.toml\"",
            "screens.edit",
        ],
    },
    // S7-35: [workspace.dependencies] (строка 11, байт 156) свёрнута — ▸ и `⋯`, номера
    // идут с 21; следующая сцена разворачивает.
    Scene {
        name: "editor-fold",
        steps: &["editor.fold at=156"],
    },
    Scene {
        name: "editor-eol-menu",
        steps: &["editor.fold at=156", "editor.eol_menu"],
    },
    // S7-18: Alt+F8 — «Перейти к строке» с набранным «29:7», курсор на месте (6:18).
    Scene {
        name: "screen-editgoto",
        steps: &["ui.cancel", "screens.edit kind=goto", "editor.goto"],
    },
    // Ctrl+F7 с regex и группами, как на макете; курсор — в начало строки 14 (`rayon`):
    // текущее совпадение — первое от него.
    Scene {
        name: "screen-editreplace",
        steps: &[
            "editor.goto to=14:1",
            "screens.edit kind=replace",
            "editor.replace",
        ],
    },
    Scene {
        name: "editor-no-toolbar",
        steps: &["editor.search_cancel", "ui.cancel", "editor.toolbar"],
    },
    Scene {
        name: "editor-ask-close",
        steps: &["editor.toolbar", "editor.close"],
    },
    Scene {
        name: "editor-closed",
        steps: &["editor.answer choice=discard", "resize 1200x760"],
    },
    // S7-41: подсветка окном — JSON ≈ 300 КБ (несколько точек), окно в середине
    // многоэкранного массива: ключи — текст, значения — по ролям, ▾ у объектов.
    Scene {
        name: "hl-big-json",
        steps: &[
            "ui.cancel",
            "resize 1280x760",
            "panel.focus side=left",
            "editor.open path=\"Projects\\urfin\\assets\\big.json\"",
            "editor.goto to=11254:1",
        ],
    },
    Scene {
        name: "hl-big-json-closed",
        steps: &["editor.close", "resize 1200x760"],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clicks_to_units() {
        assert_eq!(unit_of(1), Unit::Char);
        assert_eq!(unit_of(2), Unit::Word);
        assert_eq!(unit_of(3), Unit::Line);
        assert_eq!(unit_of(4), Unit::Line);
    }

    #[test]
    fn language_names() {
        assert_eq!(lang_name("toml"), "TOML");
        assert_eq!(lang_name("rust"), "Rust");
        assert_eq!(lang_name("csharp"), "C#");
    }
}
