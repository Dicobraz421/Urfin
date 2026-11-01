//! Экран редактора (S7-16, S7 §5.3, §6): фича окна `Editor` (ADR-0022) — `Main::Replace`
//! на месте панелей. Текст документа — `Entity<EditorState>` gpui-kit на документ (ключ —
//! `DocId`); всё остальное о документе — `EditorDoc` в `urfin-view`.
//!
//! Поток (решение 2026-10-30 №1, B1): handler кладёт текст загрузки в `EditorDoc.loaded`;
//! фича переводит Rope в строку в фоне, отдаёт её `EditorState::set_value` (на UI-потоке —
//! спайк, `docs/perf/s7.md`) и отвечает `InboxMsg::Adopted`. Повторный `loaded` (F8 без
//! правок, перечитывание) заменяет текст. Правки и курсор — `InboxMsg::Edited`; после
//! паузы — снимок `Snapshot { then: Diff }` для полосы изменений; снимок для сохранения —
//! по `UiEvent::Ext { id: "editor.snapshot" }`.
//!
//! Своё поверх `EditorState` (S7 §5.2): `editor.delete_line`, Ins — режим замены
//! (символ под курсором выделяется перед вводом), Enter и вставка — с концом строк
//! документа (`eol_new`), `editor.eol` — перевод всего текста одной правкой, undo/redo —
//! действия gpui-kit. Текст хранится с концами строк как в файле: `EditorState` их не
//! меняет (строки делит по LF, `\r` остаётся в строке), поэтому сохранение без правок
//! даёт те же байты.
//!
//! Экран: шапка 36 (путь, «только чтение», счётчик правок; загрузка — прогресс; «слишком
//! большой» — строка с действиями), панель инструментов (S7-23, [`super::toolbar`]),
//! тело — полоса изменений 3 ([`super::gutter`]), номера и свёртка gpui-kit, текст,
//! мини-карта 88 ([`super::minimap`]); внизу — вопрос документа (`EdAsk`) и строка
//! `caution` при `lossy`; статус и keybar.
//!
//! S7-18: строки «Найти» / «Заменить на» (F7, Ctrl+F7) и «Перейти к строке» (Alt+F8) над
//! статусом — [`super::edit_find`]; пока они открыты, фокус Core — `Focus::Overlay(Find)`,
//! а экран — всё ещё редактор.

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::base::input::{
    Editor as TextEditor, EditorState, Indent, InputEditorStyle, InputEvent, Outdent, Paste,
    Position, Redo, RopeExt as _, TabSize, Undo,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, ClickEvent, Context, Div, Edges, Entity, FocusHandle, Focusable as _, Keystroke,
    SharedString, Subscription, Window, div, px,
};
use rustc_hash::FxHashMap;
use urfin_core::{AppState, Args, Command, Focus, OverlayKind};
use urfin_view::{
    After, DocId, EdAsk, EdState, EditorDoc, Eol, InboxMsg, Placement, ViewState, codepage_menu,
};

use super::super::CoreHost;
use super::super::features::{Frame, Main, ShellFeature};
use super::super::pouch::{H_ITEM, Notch, place, pouch};
use super::super::screens::Scene;
use super::super::strip::{Btn, Tone, button, container};
use super::super::theme::Theme;
use super::edit_bench::{Act, EdBench};
use super::edit_find::{self, Jump};
use super::find::{BarAction, BarMode, FindBar, Flag};
use super::rows::{H_HEAD, MONO};
use super::{enc_label, gutter, head, minimap, toolbar};
use crate::tokens::{Metric, Role};

/// Пауза после правки до снимка для полосы изменений.
const DIFF_PAUSE: Duration = Duration::from_millis(300);
/// Текст короче — в строку прямо в кадре, длиннее — в фоне.
const INLINE_CONVERT: usize = 1 << 20;
/// Строка текста редактора (S7 §5.3).
const ROW_H: f32 = 20.0;
const ICON_FONT: &str = "Segoe Fluent Icons";
/// Значок «код» шапки (`</>`).
const CODE_ICON: char = '\u{E943}';
/// Пунктов «мешочка» кодировок на виду.
const VISIBLE: usize = 11;
const W_ENC: f32 = 300.0;
const W_EOL: f32 = 160.0;

/// Фича занята (передача текста, правки сцены, ждёт снимок или diff): сцена `--screens`
/// ещё не готова к снимку.
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

/// Сцена `--screens` может сниматься: текст передан, правки и diff закончены.
pub(super) fn settled(s: &AppState) -> bool {
    let Some(d) = shown(s).and_then(|id| doc(s, id)) else {
        return true;
    };
    match d.state {
        EdState::Loading { .. } | EdState::Saving(_) => false,
        EdState::Ready => d.loaded.is_none() && !BUSY.load(Ordering::Relaxed),
        EdState::TooLarge { .. } | EdState::Failed => true,
    }
}

/// Конец строки текстом.
fn eol_str(e: Eol) -> &'static str {
    match e {
        Eol::Lf | Eol::Mixed => "\n",
        Eol::CrLf => "\r\n",
        Eol::Cr => "\r",
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

/// Текст с концами строк `to` (CRLF, CR и LF — к одному виду).
pub(super) fn normalize_eol(text: &str, to: Eol) -> String {
    let lf = text.replace("\r\n", "\n").replace('\r', "\n");
    match to {
        Eol::Lf | Eol::Mixed => lf,
        Eol::CrLf => lf.replace('\n', "\r\n"),
        Eol::Cr => lf.replace('\n', "\r"),
    }
}

/// Имя языка для статуса (имена gpui-kit → как пишут люди).
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

/// Строк в тексте (последний перевод строки не начинает новую).
pub(super) fn line_count(rope: &gpui_kit::base::input::Rope) -> usize {
    let n = rope.lines_len();
    if n > 1 && rope.len() > 0 && rope.char_at(rope.len() - 1) == Some('\n') {
        n - 1
    } else {
        n
    }
}

/// Отступ строки (пробелы и табы в начале).
fn indent_of(line: &str) -> String {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Действие над текстом, которому нужно окно (выполняется в кадре).
#[derive(Clone, Debug, PartialEq)]
enum Op {
    Undo,
    Redo,
    DeleteLine,
    Indent,
    Outdent,
    Eol(Eol),
    /// `editor.goto to=…` (синтаксис проверил handler).
    Goto(String),
    /// Правки сцены `screen-editor` (`screens.edit`).
    Demo,
}

/// Что применено к `EditorState` из конфига и документа.
#[allow(clippy::struct_excessive_bools, reason = "независимые настройки")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Applied {
    readonly: bool,
    wrap: bool,
    gutter: bool,
    tab: u8,
    spaces: bool,
    theme: u64,
    font: u32,
}

/// `EditorState` документа и состояние вокруг него.
#[allow(
    clippy::struct_excessive_bools,
    reason = "независимые признаки документа"
)]
pub(super) struct DocView {
    pub(super) state: Entity<EditorState>,
    _subs: [Subscription; 2],
    /// Текст передан в `EditorState` (хоть раз).
    adopted: bool,
    /// `Adopted` на текущий `loaded` отправлен: ждём, пока handler его очистит.
    acked: bool,
    /// Строка из Rope в работе (фон).
    converting: bool,
    /// Готовая строка и время перевода.
    ready: Option<(Arc<str>, Duration)>,
    /// Есть несохранённые правки (в core — через `Edited`).
    dirty: bool,
    sent: Option<(bool, (u32, u32))>,
    /// Правок с загрузки (событий `Change`).
    edits: u64,
    changed_at: Option<Instant>,
    /// `edits` на момент последнего снимка для diff.
    snap_edits: u64,
    /// Снимок для diff отправлен — ждём новый `changes` (не дольше секунды).
    diff_wait: Option<Instant>,
    /// Изменения, которые были, когда ушёл снимок.
    snap_changes: Option<urfin_view::LineChanges>,
    /// `Change` от undo/redo фичи, которые ещё придут.
    replay: u32,
    redo: bool,
    /// Режим замены (Ins).
    overwrite: bool,
    applied: Option<Applied>,
    /// Мини-карта прошлого кадра (клик).
    map: minimap::Geo,
    /// Версия текста: растёт с каждой правкой и передачей текста (свежесть поиска).
    ver: u64,
    /// Подсветка совпадений поиска (S7-18).
    decos: Option<edit_find::Decos>,
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
pub(in crate::gpui_shell) struct Editor {
    docs: FxHashMap<DocId, DocView>,
    ops: Vec<(DocId, Op)>,
    out: Vec<InboxMsg>,
    menu: Option<Menu>,
    /// Документ, которому отдан фокус окна.
    focused: Option<DocId>,
    bench: Option<EdBench>,
    bench_checked: bool,
    /// Окно было активно в прошлом проходе (переход → `InboxMsg::Blur`).
    active: bool,
    /// Цвета и размеры для `EditorState` (кадр `input` без `Frame`): из прошлого `main`.
    theme: Option<Rc<ThemeCopy>>,
    /// Поиск, замена, переход (S7-18).
    pub(super) find: edit_find::Find,
}

/// Что `EditorState` берёт из темы окна.
pub(super) struct ThemeCopy {
    generation: u64,
    colors: [gpui_kit::Hsla; 7],
    mono: f32,
    /// Совпадения поиска: `highlight`, текущее — `accent.fill` и `accent.on`.
    hit: [gpui_kit::Hsla; 3],
}

impl ThemeCopy {
    fn of(t: &Theme) -> ThemeCopy {
        ThemeCopy {
            generation: t.generation(),
            colors: [
                t.c(Role::TextPrimary),
                t.c(Role::TextTertiary),
                t.c(Role::LayerPanel),
                t.c(Role::StrokeDivider),
                t.c(Role::AccentSubtle),
                t.c(Role::CurrentLine),
                t.c(Role::TextPrimary),
            ],
            mono: t.m(Metric::TypeMono),
            hit: [
                t.c(Role::Highlight),
                t.c(Role::AccentFill),
                t.c(Role::AccentOn),
            ],
        }
    }
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

impl DocView {
    fn on_change(&mut self) {
        self.edits += 1;
        self.ver += 1;
        self.changed_at = Some(Instant::now());
        if self.replay > 0 {
            self.replay -= 1;
        } else {
            self.redo = false;
        }
        self.dirty = true;
    }
}

impl Editor {
    /// `EditorState` нового документа: номера, свёртка, без своей панели поиска.
    fn create(
        id: DocId,
        d: &EditorDoc,
        v: &ViewState,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) -> DocView {
        let cfg = &v.editor;
        let state = cx.new(|cx| {
            EditorState::new(window, cx)
                .line_number(cfg.gutter)
                .folding(cfg.gutter)
                .searchable(false)
                .soft_wrap(cfg.wrap)
                .tab_size(TabSize {
                    tab_size: usize::from(cfg.tab_size.max(1)),
                    hard_tabs: !cfg.expand_tabs,
                })
        });
        let _ = d;
        let change = cx.subscribe_in(
            &state,
            window,
            move |h: &mut CoreHost, _, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::Change)
                    && let Some(e) = h.features.get_mut::<Editor>()
                    && let Some(dv) = e.docs.get_mut(&id)
                {
                    dv.on_change();
                    cx.notify();
                }
            },
        );
        // Курсор и прокрутка событий не дают: статус, полоса и карта — по перерисовке.
        let notify = cx.observe(&state, |_: &mut CoreHost, _, cx| cx.notify());
        DocView {
            state,
            _subs: [change, notify],
            adopted: false,
            acked: false,
            converting: false,
            ready: None,
            dirty: false,
            sent: None,
            edits: 0,
            changed_at: None,
            snap_edits: 0,
            diff_wait: None,
            snap_changes: None,
            replay: 0,
            redo: false,
            overwrite: false,
            applied: None,
            map: Rc::default(),
            ver: 0,
            decos: None,
        }
    }

    /// Шрифт и размер текста (`mono` — моно окна).
    fn font(v: &ViewState, mono: f32) -> (SharedString, f32) {
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
        (family, size)
    }

    /// Конфиг, «только чтение» и цвета темы — в `EditorState`, если изменились.
    fn apply(
        dv: &mut DocView,
        d: &EditorDoc,
        v: &ViewState,
        theme: &ThemeCopy,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) {
        let cfg = &v.editor;
        let (_, size) = Self::font(v, theme.mono);
        let now = Applied {
            readonly: d.readonly || !d.is_ready(),
            wrap: cfg.wrap,
            gutter: cfg.gutter,
            tab: cfg.tab_size,
            spaces: cfg.expand_tabs,
            theme: theme.generation,
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "px"
            )]
            font: (size * 10.0) as u32,
        };
        if dv.applied == Some(now) {
            return;
        }
        let was = dv.applied.replace(now);
        let [fg, muted, bg, border, sel, line, caret] = theme.colors;
        let style = InputEditorStyle {
            foreground: fg,
            muted_foreground: muted,
            background: bg,
            border,
            selection: sel,
            caret,
            editor_active_line: Some(line),
            editor_gutter_background: Some(bg),
            ..InputEditorStyle::default()
        };
        dv.state.update(cx, |st, cx| {
            st.set_editor_style(style);
            st.set_editor_paddings(Edges {
                top: px(4.0),
                right: px(0.0),
                bottom: px(0.0),
                left: px(0.0),
            });
            if was.is_none_or(|w| w.readonly != now.readonly) {
                st.set_readonly(now.readonly, cx);
            }
            if was.is_none_or(|w| w.wrap != now.wrap) {
                st.set_soft_wrap(now.wrap, window, cx);
            }
            if was.is_none_or(|w| w.gutter != now.gutter) {
                st.set_line_number(now.gutter, window, cx);
                st.set_folding(now.gutter, window, cx);
            }
            if was.is_none_or(|w| w.tab != now.tab || w.spaces != now.spaces) {
                st.set_tab_size(
                    TabSize {
                        tab_size: usize::from(now.tab.max(1)),
                        hard_tabs: !now.spaces,
                    },
                    cx,
                );
            }
            cx.notify();
        });
    }

    /// Текст загрузки → `EditorState`: короткий — сразу, длинный — строкой из фона.
    fn adopt(&mut self, id: DocId, d: &EditorDoc, window: &mut Window, cx: &mut Context<CoreHost>) {
        let Some(dv) = self.docs.get_mut(&id) else {
            return;
        };
        if d.loaded.is_none() {
            dv.acked = false;
            return;
        }
        if dv.acked {
            return;
        }
        if let Some(b) = self.bench.as_mut() {
            b.loaded();
        }
        let ready = match dv.ready.take() {
            Some(r) => r,
            None if dv.converting => return,
            None => {
                let Some(rope) = d.loaded.clone() else {
                    return;
                };
                if rope.len() <= INLINE_CONVERT {
                    let t = Instant::now();
                    (Arc::<str>::from(rope.to_string()), t.elapsed())
                } else {
                    dv.converting = true;
                    let task = cx.background_spawn(async move {
                        let t = Instant::now();
                        let s = Arc::<str>::from(rope.to_string());
                        (s, t.elapsed())
                    });
                    cx.spawn_in(window, async move |this, cx| {
                        let r = task.await;
                        let _ = this.update(cx, |h, cx| {
                            if let Some(e) = h.features.get_mut::<Editor>()
                                && let Some(dv) = e.docs.get_mut(&id)
                            {
                                dv.converting = false;
                                dv.ready = Some(r);
                            }
                            cx.notify();
                        });
                    })
                    .detach();
                    return;
                }
            }
        };
        let (text, conv) = ready;
        let bytes = text.len();
        let start = d.start;
        let t = Instant::now();
        dv.state.update(cx, |st, cx| {
            st.set_value(SharedString::from(text), window, cx);
            if let Some((line, col)) = start {
                st.set_cursor_position(Position::new(line, col), window, cx);
            }
        });
        let set = t.elapsed();
        tracing::debug!(
            ?id,
            bytes,
            conv_ms = conv.as_millis(),
            set_ms = set.as_millis(),
            "editor text adopted"
        );
        dv.adopted = true;
        dv.ver += 1;
        dv.acked = true;
        dv.dirty = false;
        dv.sent = None;
        dv.edits = 0;
        dv.snap_edits = 0;
        dv.changed_at = None;
        dv.redo = false;
        dv.replay = 0;
        dv.diff_wait = None;
        self.out.push(InboxMsg::Adopted(id));
        if let Some(b) = self.bench.as_mut() {
            b.bytes = bytes as u64;
            b.lines = line_count(dv.state.read(cx).text());
            b.conv_ms = conv.as_secs_f64() * 1000.0;
            b.adopted(set);
        }
    }

    /// Действия над текстом, накопленные из событий Core.
    fn run_ops(&mut self, s: &AppState, window: &mut Window, cx: &mut Context<CoreHost>) {
        let mut later = Vec::new();
        for (id, op) in std::mem::take(&mut self.ops) {
            let Some(d) = doc(s, id) else {
                continue;
            };
            let Some(dv) = self.docs.get_mut(&id) else {
                continue;
            };
            // Текст ещё не передан: действие — после него.
            if !dv.adopted || dv.ready.is_some() || dv.converting {
                if matches!(d.state, EdState::Loading { .. } | EdState::Ready) {
                    later.push((id, op));
                }
                continue;
            }
            let editable = d.is_ready() && !d.readonly;
            let st = dv.state.clone();
            let focus = st.read(cx).focus_handle(cx);
            match op {
                Op::Undo | Op::Redo if editable => {
                    dv.replay += 1;
                    if op == Op::Undo {
                        dv.redo = true;
                        focus.dispatch_action(&Undo, window, cx);
                    } else {
                        focus.dispatch_action(&Redo, window, cx);
                    }
                }
                Op::Indent if editable => focus.dispatch_action(&Indent, window, cx),
                Op::Outdent if editable => focus.dispatch_action(&Outdent, window, cx),
                Op::DeleteLine if editable => st.update(cx, |st, cx| {
                    let rope = st.text();
                    let line = st.cursor_position().line as usize;
                    let start = rope.line_start_offset(line);
                    let mut end = rope.line_end_offset(line);
                    // `line_end_offset` — до `\n` включительно, кроме последней строки.
                    if end < rope.len() && rope.char_at(end) == Some('\n') {
                        end += 1;
                    }
                    if end > start {
                        st.set_selected_range(start..end, cx);
                        st.replace("", window, cx);
                    }
                }),
                Op::Eol(to) if editable => st.update(cx, |st, cx| {
                    let text = st.text().to_string();
                    let new = normalize_eol(&text, to);
                    if new != text {
                        let pos = st.cursor_position();
                        let scroll = st.scroll_offset();
                        st.replace_all(new, window, cx);
                        st.set_cursor_position(pos, window, cx);
                        st.set_scroll_offset(scroll, cx);
                    }
                }),
                Op::Goto(to) => {
                    if let Ok(g) = urfin_view::goto::parse(&to) {
                        st.update(cx, |st, cx| {
                            let line = st.cursor_position().line;
                            let at = edit_find::goto_offset(g, st.text(), line);
                            let pos = st.text().offset_to_position(at);
                            st.set_cursor_position(pos, window, cx);
                        });
                    }
                }
                Op::Demo => st.update(cx, |st, cx| demo_edits(st, window, cx)),
                _ => {}
            }
        }
        self.ops = later;
    }

    /// Правки и курсор → `Edited`; пауза после правок → снимок для diff.
    fn report(&mut self, id: DocId, d: &EditorDoc, cx: &Context<CoreHost>) {
        let Some(dv) = self.docs.get_mut(&id) else {
            return;
        };
        if !dv.adopted {
            return;
        }
        let st = dv.state.read(cx);
        // Diff ответил: изменений нет — текст как сохранённый (undo до конца).
        if let Some(prev) = &dv.snap_changes
            && *prev != d.changes
        {
            dv.diff_wait = None;
            dv.snap_changes = None;
            if d.changes.is_empty() && dv.snap_edits == dv.edits {
                dv.dirty = false;
            }
        }
        if dv
            .diff_wait
            .is_some_and(|t| t.elapsed() > Duration::from_secs(1))
        {
            dv.diff_wait = None;
            dv.snap_changes = None;
        }
        let p = st.cursor_position();
        let now = (dv.dirty, (p.line, p.character));
        if dv.sent != Some(now) {
            dv.sent = Some(now);
            self.out.push(InboxMsg::Edited {
                doc: id,
                dirty: now.0,
                cursor: now.1,
            });
        }
        if dv.edits != dv.snap_edits && dv.changed_at.is_some_and(|t| t.elapsed() >= DIFF_PAUSE) {
            dv.snap_edits = dv.edits;
            dv.diff_wait = Some(Instant::now());
            dv.snap_changes = Some(d.changes.clone());
            self.out.push(InboxMsg::Snapshot {
                doc: id,
                text: st.text().clone(),
                then: After::Diff,
            });
        }
    }

    /// Шаг замера `--bench-editor`.
    fn bench_step(&mut self, id: DocId, window: &mut Window, cx: &mut Context<CoreHost>) {
        let Some(b) = self.bench.as_mut() else {
            return;
        };
        if b.waiting() {
            return;
        }
        let act = b.frame();
        window.request_animation_frame();
        let Some(dv) = self.docs.get(&id) else {
            return;
        };
        dv.state.update(cx, |st, cx| match act {
            Act::None => {}
            Act::Down(n) => {
                let p = st.cursor_position();
                st.set_cursor_position(Position::new(p.line + n, 0), window, cx);
            }
            Act::Type => st.insert("x", window, cx),
            Act::Jump(per_mille) => {
                let total = u32::try_from(st.text().lines_len()).unwrap_or(u32::MAX);
                let line =
                    u32::try_from(u64::from(total) * u64::from(per_mille) / 1000).unwrap_or(0);
                st.set_cursor_position(Position::new(line, 0), window, cx);
            }
            Act::Exit => {
                use std::io::Write as _;
                let _ = std::io::stdout().flush();
                std::process::exit(0);
            }
        });
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

    /// Enter в документе с CRLF или CR: перевод строки документа и отступ текущей строки
    /// (gpui-kit вставляет `\n`).
    fn newline(dv: &DocView, eol: Eol, window: &mut Window, cx: &mut Context<CoreHost>) {
        dv.state.update(cx, |st, cx| {
            let line = st.cursor_position().line as usize;
            let indent = indent_of(&st.text().slice_line(line).to_string());
            st.replace(format!("{}{indent}", eol_str(eol)), window, cx);
        });
    }

    /// Режим замены: печатный символ заменяет символ под курсором — выделить его до
    /// того, как gpui-kit примет ввод.
    fn overtype(dv: &DocView, cx: &mut Context<CoreHost>) {
        dv.state.update(cx, |st, cx| {
            let r = st.selected_range();
            if !r.is_empty() {
                return;
            }
            let rope = st.text();
            let Some(ch) = rope.char_at(r.start) else {
                return;
            };
            if ch == '\n' || ch == '\r' {
                return;
            }
            st.set_selected_range(r.start..r.start + ch.len_utf8(), cx);
        });
    }
}

impl Editor {
    /// Строка документа `id` открыта (фокус Core — у неё).
    fn bar_open(&self, s: &AppState, id: DocId) -> bool {
        self.find
            .bar_for(id, s.focus == Focus::Overlay(OverlayKind::Find))
            .is_some()
    }

    /// F7 / Ctrl+F7 / Alt+F8: открыть строку (или сменить ряд открытой). Текст «Найти» —
    /// выделение в одну строку, иначе прошлый образец.
    fn open_bar(&mut self, id: DocId, mode: BarMode, replace: bool, cx: &Context<CoreHost>) {
        let demo = self.find.demo.take();
        let f = &mut self.find;
        let selected = self.docs.get(&id).and_then(|dv| {
            let t = dv.state.read(cx).selected_text().to_string();
            (!t.is_empty() && !t.contains('\n') && t.chars().count() <= 256).then_some(t)
        });
        let mut flags = f.last.as_ref().map(edit_find::flags_of).unwrap_or_default();
        let (find_init, repl_init) = match &demo {
            Some((text, repl)) => {
                if repl.is_some() {
                    flags.regex = true;
                }
                (text.clone(), repl.clone().unwrap_or_default())
            }
            None => (
                selected
                    .or_else(|| f.last.as_ref().map(|q| q.text.clone()))
                    .unwrap_or_default(),
                f.last_repl.clone(),
            ),
        };
        let is_demo = demo.is_some();
        let init = match mode {
            BarMode::Find => find_init,
            BarMode::Goto => demo.map(|d| d.0).unwrap_or_default(),
        };
        match f.bar.as_mut().filter(|b| b.owner == id.0) {
            // Открытая строка: другой ряд — с новым текстом; тот же — как есть.
            Some(b) => {
                if b.mode != mode || is_demo {
                    b.switch(mode, init);
                }
                if is_demo {
                    b.flags = flags;
                }
            }
            None => {
                f.bar = Some(FindBar::editor(mode, id.0, init, flags, None));
            }
        }
        if replace && let Some(b) = f.bar.as_mut() {
            // Фокус — «Найти», как на макете; Tab — к замене.
            b.open_replace(repl_init, false);
        }
    }

    /// Действие строки → команды Core (окно — для фокуса полей и замены).
    fn bar_action(
        &mut self,
        s: &AppState,
        id: DocId,
        act: BarAction,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) -> Vec<Command> {
        let Some(bar) = self.find.bar.as_mut() else {
            return Vec::new();
        };
        match act {
            BarAction::Close => vec![Command::new("editor.search_cancel")],
            BarAction::Swallow => Vec::new(),
            BarAction::Toggle(f) => {
                bar.flags.toggle(f);
                Vec::new()
            }
            BarAction::Tab => {
                bar.focus_other(window, cx);
                Vec::new()
            }
            BarAction::OpenReplace => {
                if bar.mode != BarMode::Find {
                    let init = self.find.last.as_ref().map(|q| q.text.clone());
                    bar.switch(BarMode::Find, init.unwrap_or_default());
                }
                let repl = self.find.last_repl.clone();
                if let Some(b) = self.find.bar.as_mut() {
                    b.open_replace(repl, true);
                }
                Vec::new()
            }
            BarAction::Switch(mode) => {
                let init = match mode {
                    BarMode::Find => self.find.last.as_ref().map(|q| q.text.clone()),
                    BarMode::Goto => None,
                };
                bar.switch(mode, init.unwrap_or_default());
                Vec::new()
            }
            BarAction::Again { back } | BarAction::Commit { back } => match bar.mode {
                BarMode::Find => {
                    let q = edit_find::query(bar);
                    if !q.text.is_empty() {
                        self.find.last = Some(q);
                        self.find.jump = Some(Jump::Next { back });
                    }
                    Vec::new()
                }
                BarMode::Goto => {
                    let text = bar.text();
                    if urfin_view::goto::parse(&text).is_err() {
                        return Vec::new();
                    }
                    vec![Command::new("editor.goto").arg("to", text.trim())]
                }
            },
            BarAction::Replace { all } => {
                self.replace(s, id, all, window, cx);
                Vec::new()
            }
        }
    }

    /// «Заменить» (выделенное совпадение — и к следующему) или «Заменить все».
    fn replace(
        &mut self,
        s: &AppState,
        id: DocId,
        all: bool,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) {
        let Some(d) = doc(s, id) else {
            return;
        };
        let (Some(bar), Some(dv)) = (self.find.bar.as_ref(), self.docs.get(&id)) else {
            return;
        };
        if !d.is_ready() || d.readonly || !dv.adopted || self.find.replacing {
            return;
        }
        let q = edit_find::query(bar);
        let tpl = bar.repl_text();
        self.find.last = Some(q.clone());
        self.find.last_repl.clone_from(&tpl);
        let Some(h) = self.find.fresh(id, dv.ver).filter(|h| h.q == q) else {
            return;
        };
        let Some(finder) = h.finder.clone() else {
            return;
        };
        let rope = dv.state.read(cx).text().clone();
        if all {
            let ver = dv.ver;
            self.find
                .replace_all((id, ver), finder, tpl, &rope, window, cx);
            return;
        }
        let sel = dv.state.read(cx).selected_range();
        if edit_find::ordinal(&h.found.ranges, &sel).is_some()
            && let Some(text) = finder.expand(&rope, &sel, &tpl)
        {
            dv.state.update(cx, |st, cx| st.replace(text, window, cx));
        }
        self.find.jump = Some(Jump::Next { back: false });
    }

    /// Кадр поиска: строка (поля, закрытие), итог фона, «Заменить все», поиск по смене
    /// образца или текста, переход к совпадению, подсветка.
    fn find_frame(
        &mut self,
        s: &AppState,
        id: DocId,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) {
        let open = self.bar_open(s, id);
        if open {
            if let Some(b) = self.find.bar.as_mut() {
                b.sync(window, cx);
            }
        } else if self.find.bar.take().is_some() {
            self.find.closed();
            // Фокус — снова тексту.
            self.focused = None;
        }
        self.find.take_result();
        let Some(dv) = self.docs.get_mut(&id) else {
            return;
        };
        if !dv.adopted {
            return;
        }
        if let Some(r) = self.find.replaced.take()
            && r.doc == id
            && r.ver == dv.ver
        {
            if r.n > 0 {
                dv.state.update(cx, |st, cx| {
                    let pos = st.cursor_position();
                    let scroll = st.scroll_offset();
                    st.replace_all(r.text, window, cx);
                    st.set_cursor_position(pos, window, cx);
                    st.set_scroll_offset(scroll, cx);
                });
            }
            let mut a = urfin_i18n::FluentArgs::new();
            a.set("count", r.n as u64);
            self.find.note = Some(urfin_i18n::current().fmt("ui-replace-done", &a));
        }
        let ver = dv.ver;
        let rope = dv.state.read(cx).text().clone();
        let bar_q = self
            .find
            .bar
            .as_ref()
            .filter(|b| open && b.mode == BarMode::Find)
            .map(edit_find::query);
        let want = match &bar_q {
            Some(q) => {
                let anchor = dv.state.read(cx).selected_range().start;
                self.find.track_query(q, anchor);
                if !q.text.is_empty() {
                    self.find.last = Some(q.clone());
                }
                Some(q.clone())
            }
            None if self.find.jump.is_some() => self.find.last.clone(),
            None => None,
        };
        if let Some(q) = want.filter(|q| !q.text.is_empty()) {
            self.find.want(id, &q, ver, &rope, window, cx);
        } else if bar_q.is_some() {
            self.find.jump = None;
        }
        self.find.do_jump(id, ver, &dv.state, cx);
        let show = bar_q.is_some_and(|q| !q.text.is_empty());
        if let Some(t) = self.theme.as_ref() {
            self.find
                .paint(&mut dv.decos, id, show, &dv.state, t.hit, t.generation, cx);
        }
    }
}

/// Правки сцены `screen-editor`: версия 0.4.1 → 0.4.2 (строка 6), две новые строки
/// зависимостей (17–18), курсор — в конец строки 6 (как на макете).
fn demo_edits(st: &mut EditorState, window: &mut Window, cx: &mut Context<EditorState>) {
    let rope = st.text();
    let six = rope.line_start_offset(5);
    let line = rope.slice_line(5).to_string();
    if let Some(k) = line.find("0.4.1") {
        let at = six + k + 4;
        st.set_selected_range(at..at + 1, cx);
        st.replace("2", window, cx);
    }
    let at = st.text().line_start_offset(16);
    st.set_selected_range(at..at, cx);
    st.replace("parking_lot = \"0.12\"\nsmallvec = \"1.13\"\n", window, cx);
    st.set_cursor_position(Position::new(5, 17), window, cx);
}

impl ShellFeature for Editor {
    #[allow(
        clippy::too_many_lines,
        reason = "события Core фичи: правки, строки, меню"
    )]
    fn ext(
        &mut self,
        id: &str,
        args: &Args,
        s: &AppState,
        cx: &mut Context<CoreHost>,
    ) -> Option<Vec<Command>> {
        let op = match id {
            "editor.undo" => Some(Op::Undo),
            "editor.redo" => Some(Op::Redo),
            "editor.delete_line" => Some(Op::DeleteLine),
            "editor.block.shift_left" => Some(Op::Outdent),
            "editor.block.shift_right" => Some(Op::Indent),
            "editor.eol" => Some(Op::Eol(match args.str("to")? {
                "crlf" => Eol::CrLf,
                "cr" => Eol::Cr,
                _ => Eol::Lf,
            })),
            "screens.edit" if args.str("kind").is_none() => Some(Op::Demo),
            _ => None,
        };
        if let Some(op) = op {
            let doc = urfin_view::handler::ext_doc(args).or_else(|| shown(s))?;
            self.ops.push((doc, op));
            cx.notify();
            return Some(Vec::new());
        }
        match id {
            // Сцены S7-18: тексты строки перехода и замены, как на макетах.
            "screens.edit" => {
                self.find.demo = match args.str("kind")? {
                    "goto" => Some(("29:7".to_owned(), None)),
                    _ => Some((
                        r#"^(\w+) = "([\d.]+)""#.to_owned(),
                        Some(r#"$1 = { version = "$2" }"#.to_owned()),
                    )),
                };
                return Some(Vec::new());
            }
            "editor.search" | "editor.replace" => {
                let doc = urfin_view::handler::ext_doc(args)?;
                self.menu = None;
                self.open_bar(doc, BarMode::Find, id == "editor.replace", cx);
                cx.notify();
                return Some(Vec::new());
            }
            "editor.goto" => {
                let doc = urfin_view::handler::ext_doc(args)?;
                self.menu = None;
                match args.str("to") {
                    Some(to) => self.ops.push((doc, Op::Goto(to.to_owned()))),
                    None => self.open_bar(doc, BarMode::Goto, false, cx),
                }
                cx.notify();
                return Some(Vec::new());
            }
            "editor.search_next" | "editor.search_prev" => {
                if self.find.last.is_none() {
                    return Some(vec![Command::new("editor.search")]);
                }
                self.find.jump = Some(Jump::Next {
                    back: id == "editor.search_prev",
                });
                cx.notify();
                return Some(Vec::new());
            }
            // Строку закрывает смена фокуса (кадр `input`).
            "editor.search_cancel" => {
                cx.notify();
                return Some(Vec::new());
            }
            _ => {}
        }
        match id {
            // Перенос и панель — из `[editor]` в кадре (`apply`).
            "editor.wrap" | "editor.toolbar" => {
                // Панель спрятана — «мешочка» от её кнопки больше нет.
                self.menu = None;
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
            "editor.snapshot" => {
                let doc = urfin_view::handler::ext_doc(args)?;
                let then = After::from_args(args)?;
                let dv = self.docs.get(&doc)?;
                let text = dv.state.read(cx).text().clone();
                self.out.push(InboxMsg::Snapshot { doc, text, then });
                Some(Vec::new())
            }
            "editor.saved" => {
                let doc = urfin_view::handler::ext_doc(args)?;
                if let Some(dv) = self.docs.get_mut(&doc) {
                    dv.dirty = false;
                    dv.sent = None;
                }
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
            let cmds = self.bar_action(s, id, act, window, cx);
            cx.notify();
            return Some(cmds);
        }
        let d = doc(s, id)?;
        if let Some(ask) = d.ask {
            return Some(Self::ask_key(ask, id, ks));
        }
        let dv = self.docs.get_mut(&id)?;
        if !dv.adopted || !d.is_ready() {
            return None;
        }
        let m = ks.modifiers;
        let plain = !m.control && !m.alt && !m.platform && !m.shift;
        if ks.key == "insert" && plain {
            dv.overwrite = !dv.overwrite;
            cx.notify();
            return Some(Vec::new());
        }
        if d.readonly {
            return None;
        }
        if ks.key == "enter" && plain && matches!(d.eol_new, Eol::CrLf | Eol::Cr) {
            Self::newline(dv, d.eol_new, window, cx);
            return Some(Vec::new());
        }
        // Печатный символ (AltGr = Ctrl+Alt тоже печатает).
        let typed = ks
            .key_char
            .as_deref()
            .is_some_and(|t| !t.chars().any(char::is_control))
            && (!m.control || m.alt)
            && !m.platform;
        if dv.overwrite && typed {
            Self::overtype(dv, cx);
        }
        None
    }

    fn editing(&self, s: &AppState) -> bool {
        shown(s).is_some_and(|id| {
            self.bar_open(s, id) && self.find.bar.as_ref().is_some_and(FindBar::has_field)
        })
    }

    fn inbox(&mut self, s: &AppState, _theme: &Theme, window: &mut Window) -> bool {
        // Окно потеряло фокус: автосохранение `focus` (S7 §5.4) — событием handler'у.
        let active = window.is_window_active();
        if std::mem::replace(&mut self.active, active) && !active && shown(s).is_some() {
            self.out.push(InboxMsg::Blur);
        }
        if self.out.is_empty() {
            return false;
        }
        let Some(v) = s.ext.get::<ViewState>() else {
            self.out.clear();
            return false;
        };
        let tx = v.inbox.sender();
        for m in self.out.drain(..) {
            tx.send(m);
        }
        true
    }

    /// Пауза после правок — снимок для diff (будильник кадра).
    fn tick(&mut self, _s: &AppState, now: Instant) -> Option<Duration> {
        self.docs
            .values()
            .filter(|dv| dv.edits != dv.snap_edits)
            .filter_map(|dv| dv.changed_at)
            .map(|t| (t + DIFF_PAUSE).saturating_duration_since(now))
            .min()
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
        self.docs.retain(|id, _| v.editor(*id).is_some());
        if self.menu.is_some_and(|m| v.editor(m.doc).is_none()) {
            self.menu = None;
        }
        let Some(id) = shown(s) else {
            if self.focused.take().is_some() {
                window.focus(focus, cx);
            }
            if self.find.bar.take().is_some() {
                self.find.closed();
            }
            BUSY.store(false, Ordering::Relaxed);
            return;
        };
        let Some(d) = v.editor(id) else {
            return;
        };
        if let std::collections::hash_map::Entry::Vacant(e) = self.docs.entry(id) {
            e.insert(Self::create(id, d, v, window, cx));
        }
        self.adopt(id, d, window, cx);
        if let (Some(dv), Some(theme)) = (self.docs.get_mut(&id), self.theme.as_ref()) {
            Self::apply(dv, d, v, theme, window, cx);
        } else {
            // Тема приходит с первым кадром экрана (`main`).
            window.request_animation_frame();
        }
        self.run_ops(s, window, cx);
        self.find_frame(s, id, window, cx);
        self.report(id, d, cx);
        // Фокус — тексту: при открытии экрана и когда он вернулся окну (клик мимо); не
        // пока открыта строка поиска (фокус у её поля).
        if let Some(dv) = self.docs.get(&id)
            && dv.adopted
            && !self.bar_open(s, id)
            && (self.focused != Some(id) || focus.is_focused(window))
        {
            let st = dv.state.clone();
            st.update(cx, |st, cx| st.focus(window, cx));
            self.focused = Some(id);
        }
        self.bench_step(id, window, cx);
        let busy = self.docs.get(&id).is_some_and(|dv| {
            dv.converting
                || dv.ready.is_some()
                || dv.edits != dv.snap_edits
                || dv.diff_wait.is_some()
        }) || !self.ops.is_empty()
            || self.find.busy()
            || self.find.jump.is_some();
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
        if f.s
            .ext
            .get::<ViewState>()
            .and_then(|v| v.editor(id))
            .is_some_and(|d| d.ask.is_some())
        {
            // Вопрос внизу забирает клавиши: меню закрыто.
            self.menu = None;
        }
        if self
            .theme
            .as_ref()
            .is_none_or(|t| t.generation != f.theme.generation())
        {
            self.theme = Some(Rc::new(ThemeCopy::of(f.theme)));
            window.request_animation_frame();
        }
        let v = f.s.ext.get::<ViewState>()?;
        let d = v.editor(id)?;
        let theme = f.theme;
        let narrow = f32::from(window.viewport_size().width) < super::super::chrome::NARROW_W;
        let measure = head::Measure::new(window);
        let dv = self.docs.get(&id);
        let live = dv.filter(|dv| {
            dv.adopted && d.loaded.is_none() && d.is_ready()
                || matches!(d.state, EdState::Saving(_)) && dv.adopted
        });
        let i18n = urfin_i18n::current();
        let tb = toolbar::Tb {
            dirty: d.dirty || d.is_new,
            undo: dv.is_some_and(|dv| dv.edits > 0),
            redo: dv.is_some_and(|dv| dv.redo),
            wrap: v.editor.wrap,
            readonly: d.readonly,
            enc: enc_label(d.enc.enc.name()),
            eol: eol_label(d.eol),
        };
        let show_tb = v.editor.toolbar && live.is_some();
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
        // Строка «Найти» / «Перейти» (S7-18) и метки её совпадений на мини-карте.
        let bar = self
            .find
            .bar_for(id, f.s.focus == Focus::Overlay(OverlayKind::Find));
        let marks = bar
            .filter(|b| b.mode == BarMode::Find && !b.text().is_empty())
            .and_then(|_| self.find.hits.as_ref().filter(|h| h.doc == id))
            .map(|h| Arc::clone(&h.found));
        // Тело: текст с полосой и картой, пока документ готов; иначе пусто.
        let body = match live {
            Some(dv) => {
                let (family, size) = Self::font(v, theme.m(Metric::TypeMono));
                let gutter_on = v.editor.gutter;
                let state = dv.state.clone();
                let eol = d.eol_new;
                let paste_state = dv.state.clone();
                let mut row = div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .flex()
                    .child(div().w(px(gutter::W)).flex_none())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .font_family(family)
                            .text_size(px(size))
                            .line_height(px(ROW_H))
                            .text_color(theme.c(Role::TextPrimary))
                            // Вставка — с концом строк документа (S7 §5.2).
                            .capture_action(move |_: &Paste, window, cx| {
                                let Some(text) = cx.read_from_clipboard().and_then(|c| c.text())
                                else {
                                    return;
                                };
                                let norm = normalize_eol(&text, eol);
                                if norm == text {
                                    return;
                                }
                                cx.stop_propagation();
                                paste_state.update(cx, |st, cx| {
                                    if st.is_editable() {
                                        st.replace(norm, window, cx);
                                    }
                                });
                            })
                            .child(TextEditor::new(&state)),
                    );
                if gutter_on {
                    let map_state = dv.state.clone();
                    let geo = dv.map.clone();
                    row = row.child(
                        div()
                            .id("editor-map")
                            .w(px(minimap::W))
                            .flex_none()
                            .h_full()
                            .border_l_1()
                            .border_color(theme.c(Role::StrokeDivider))
                            .child(minimap::map(
                                &dv.state,
                                &dv.map,
                                usize::from(v.editor.tab_size),
                                theme,
                                marks.clone(),
                            ))
                            .on_mouse_down(
                                gpui_kit::MouseButton::Left,
                                cx.listener(move |_, ev: &gpui_kit::MouseDownEvent, w, cx| {
                                    let at = minimap::line_at(geo.get(), f32::from(ev.position.y));
                                    let row = u32::try_from(at).unwrap_or(u32::MAX);
                                    map_state.update(cx, |st, cx| {
                                        st.set_cursor_position(Position::new(row, 0), w, cx);
                                    });
                                }),
                            ),
                    );
                }
                // Полоса — после текста: раскладка этого кадра.
                row.child(gutter::bar(&dv.state, &d.changes, v.editor.wrap, theme))
                    .into_any_element()
            }
            None => div().flex_1().into_any_element(),
        };
        root = root.child(body);
        if let Some(ask) = d.ask {
            root = root.child(ask_row(ask, d, id, theme, f.click));
        } else if d.lossy && d.is_ready() {
            root = root.child(lossy_row(d, theme, f.click));
        }
        let st = live.map(|dv| dv.state.read(cx));
        if let (Some(b), Some(st), None) = (bar, st, d.ask) {
            let editable = d.is_ready() && !d.readonly;
            root = root.child(bar_row(b, &self.find, id, st, editable, f, cx));
        }
        let zones = status(d, st, dv, v, show_tb);
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
            ));
        // Загрузка: полоса 2 px `accent.fill` под шапкой.
        if let EdState::Loading { done, total } = d.state
            && total > 0
        {
            #[allow(clippy::cast_precision_loss, reason = "доля")]
            let frac = (done as f32 / total as f32).clamp(0.0, 1.0);
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

/// Строка «Найти» (+ «Заменить на») или «Перейти к строке» документа `id`.
fn bar_row(
    b: &FindBar,
    find: &edit_find::Find,
    id: DocId,
    st: &EditorState,
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
                find.counter(id, &st.selected_range())
            };
            let replace = |all: bool| -> super::super::overlays::Handler {
                Box::new(cx.listener(move |h, _: &ClickEvent, w, cx| {
                    let Some(core) = h.core.as_ref() else {
                        return;
                    };
                    if let Some(e) = h.features.get_mut::<Editor>() {
                        e.replace(core.state(), id, all, w, cx);
                    }
                    cx.notify();
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
                a.set("total", line_count(st.text()) as u64);
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

/// Статус: «Стр 6, Кол 18» · «41 строка» · «TOML · Пробелы: 4 · Вставка» (без панели —
/// ещё кодировка и конец строки, №61).
fn status(
    d: &EditorDoc,
    st: Option<&EditorState>,
    dv: Option<&DocView>,
    v: &ViewState,
    toolbar: bool,
) -> [String; 3] {
    let i18n = urfin_i18n::current();
    let mut a = urfin_i18n::FluentArgs::new();
    let Some(st) = st else {
        return [String::new(), String::new(), String::new()];
    };
    let p = st.cursor_position();
    a.set("line", u64::from(p.line) + 1);
    a.set("col", u64::from(p.character) + 1);
    let pos = i18n.fmt("ui-edit-pos", &a);
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("count", line_count(st.text()) as u64);
    let lines = i18n.fmt("ui-edit-lines", &a);
    let mut right = Vec::new();
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
    right.push(i18n.get(if dv.is_some_and(|dv| dv.overwrite) {
        "ui-edit-overtype"
    } else {
        "ui-edit-insert"
    }));
    [pos, lines, right.join(" · ")]
}

/// Шапка 36: кнопка «История» (Alt+F11, S7-25), значок, путь к папке, «только чтение»;
/// справа счётчик правок, загрузка или строка «слишком большой» с действиями.
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
        row = row.child(chip(theme, i18n.get("ui-view-readonly")));
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
        EdState::TooLarge { size } => {
            let mut a = urfin_i18n::FluentArgs::new();
            a.set("size", urfin_i18n::fmt::bytes(size));
            let path = urfin_core::Location::clone(&d.loc);
            row =
                row.child(div().w(px(8.0)))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(13.0))
                            .child(SharedString::from(i18n.fmt("ui-edit-too-big", &a))),
                    )
                    .child(
                        small_button(theme, "edit-to-viewer", i18n.get("ui-edit-open-viewer"))
                            .on_click(click(Command::new("editor.to_viewer"))),
                    )
                    .child(
                        small_button(theme, "edit-external", i18n.get("ui-edit-external"))
                            .on_click((click_all_of(click))(vec![
                                Command::new("editor.close"),
                                Command::new("editor.open")
                                    .arg("path", path)
                                    .arg("alt", true),
                            ])),
                    );
        }
        _ if !d.changes.is_empty() => {
            row = row
                .child(div().flex_1())
                .child(chip(theme, changes_text(d)));
        }
        _ => {}
    }
    row
}

/// Несколько команд одним кликом через `click` (по очереди).
fn click_all_of(
    click: super::super::overlays::Click<'_>,
) -> impl Fn(Vec<Command>) -> super::super::overlays::Handler + '_ {
    move |cmds: Vec<Command>| {
        let hs: Vec<_> = cmds.into_iter().map(click).collect();
        Box::new(
            move |ev: &ClickEvent, w: &mut Window, cx: &mut gpui_kit::App| {
                for h in &hs {
                    h(ev, w, cx);
                }
            },
        )
    }
}

/// «+2 добавлено · 1 изменена · −1 удалено».
fn changes_text(d: &EditorDoc) -> String {
    let i18n = urfin_i18n::current();
    let c = &d.changes;
    let mut parts = Vec::new();
    for (n, key) in [
        (c.added, "ui-edit-added"),
        (c.modified, "ui-edit-changed"),
        (c.deleted, "ui-edit-removed"),
    ] {
        if n > 0 {
            let mut a = urfin_i18n::FluentArgs::new();
            a.set("count", u64::from(n));
            parts.push(i18n.fmt(key, &a));
        }
    }
    parts.join(" · ")
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

fn small_button(theme: &Theme, id: &'static str, label: String) -> gpui_kit::Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(26.0))
        .px(px(12.0))
        .flex()
        .items_center()
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.c(Role::StrokeControl))
        .bg(theme.c(Role::LayerPanel))
        .text_size(px(13.0))
        .hover(|s| s.bg(theme.c(Role::FillSubtleHover)))
        .child(SharedString::from(label))
}

/// Строка вопроса документа (S7 §5.4): текст и кнопки сразу за ним; первая — основная.
fn ask_row(
    ask: EdAsk,
    d: &EditorDoc,
    id: DocId,
    theme: &Theme,
    click: super::super::overlays::Click<'_>,
) -> Div {
    let i18n = urfin_i18n::current();
    let tone = match ask {
        EdAsk::Changed | EdAsk::Unmappable { .. } => Tone::Waiting,
        EdAsk::Close | EdAsk::ReadOnly => Tone::Plain,
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

/// Строка `caution` о недекодируемых байтах (S7 §5.1).
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
                .child(SharedString::from(i18n.fmt("ui-edit-lossy", &a))),
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

/// Сцены редактора (S7-16, S7-23): `screen-editor` — Cargo.toml с правками (счётчик,
/// полоса изменений, ●), панель инструментов, курсор на строке 6; без панели — кодировка
/// и EOL в статусе; «мешочек» концов строк; вопрос при закрытии с правками.
pub(in crate::gpui_shell) const SCENES: &[Scene] = &[
    Scene {
        name: "screen-editor",
        steps: &[
            "ui.cancel",
            "resize 1280x760",
            "panel.focus side=left",
            "editor.open path=\"Projects\\urfin\\Cargo.toml\"",
            "screens.edit",
        ],
    },
    Scene {
        name: "editor-eol-menu",
        steps: &["editor.eol_menu"],
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
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eol_normalization() {
        assert_eq!(normalize_eol("a\r\nb\nc\rd", Eol::Lf), "a\nb\nc\nd");
        assert_eq!(normalize_eol("a\nb", Eol::CrLf), "a\r\nb");
        assert_eq!(normalize_eol("a\r\nb", Eol::Cr), "a\rb");
        assert_eq!(normalize_eol("a\r\nb", Eol::CrLf), "a\r\nb");
    }

    #[test]
    fn lines_and_indent() {
        use gpui_kit::base::input::Rope;
        assert_eq!(line_count(&Rope::from("a\nb\n")), 2);
        assert_eq!(line_count(&Rope::from("a\nb")), 2);
        assert_eq!(line_count(&Rope::from("")), 1);
        assert_eq!(indent_of("  \tx y"), "  \t");
        assert_eq!(lang_name("toml"), "TOML");
        assert_eq!(lang_name("rust"), "Rust");
    }
}
