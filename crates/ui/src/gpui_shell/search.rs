//! Экран поиска (S8 §2.5, `screen-search`, S8-07): фича окна `Main::Replace` на месте панелей.
//! Слева форма 2 ряда, виртуальный список «Файл | Совпадение» и итог; справа предпросмотр
//! (S8-09): шапка 30 и документ области панели (`viewer.quick { side, path, line, hit }`
//! через 50 мс после курсора), рисует его фича вьювера.
//!
//! Экран открывает и закрывает `UiEvent::Ext { id: "search.screen" }` от handler'а `search`.
//! Фокус Core — `Focus::Overlay(Search)`: привязки экрана (стрелки, F3, F4, Esc) — в keymap
//! (`overlay && search_screen && !qs`), а клавиши правки текста идут полю мимо keymap
//! ([`field_key`]). Enter различает сам экран: в форме — `search.run`, в списке — `search.goto`.
//!
//! Модель — `urfin_core::search::SearchModel`, строки — окно стора: кадр читает только видимое.
//! Обход будит окно `MsgSink::poke`, [`Search::tick`] ставит кадр, пока стор растёт.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use compact_str::CompactString;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Bounds, ClickEvent, ContentMask, Context, Div, Entity, FocusHandle, Font,
    Hsla, Keystroke, MouseButton, MouseDownEvent, Pixels, ScrollWheelEvent, SharedString, Stateful,
    Subscription, TextAlign, TextRun, TruncateFrom, WeakEntity, Window, canvas, div, point, px,
    size,
};
use rustc_hash::FxHashMap;
use urfin_core::search::{self, Hit, SearchId, SearchModel, SearchStore};
use urfin_core::{AppState, Args, Command, Focus, Location, OverlayKind, Side};
use urfin_keymap::ev::Key;
use urfin_view::ViewState;

use super::CoreHost;
use super::chrome::SshBottom;
use super::features::{FeatureField, Features, Frame, Main, ShellFeature};
use super::panel::Listen;
use super::screens::Scene;
use super::search_bench::{Act, SearchBench};
use super::theme::Theme;
use super::view::Follow;
use crate::field_history::FieldKind;
use crate::search_view::{self, Form, Kinds};
use crate::tokens::{Metric, Role};

/// Подпись ряда формы.
const W_LABEL: f32 = 44.0;
/// Поле «Имя».
const W_NAME: f32 = 120.0;
const H_FIELD: f32 = 28.0;
const H_CHIP: f32 = 26.0;
const FORM_PAD_Y: f32 = 10.0;
const FORM_GAP: f32 = 8.0;
/// Шапка списка.
const H_HEAD: f32 = 24.0;
/// Колонка «Файл»; в строке за ней отступ, в шапке его нет (как на макете).
const W_FILE: f32 = 270.0;
const ROW_GAP: f32 = 12.0;
const TEXT: f32 = 12.5;
const ROW_TEXT: f32 = 13.0;
const LABEL_TEXT: f32 = 12.0;
const HEAD_TEXT: f32 = 11.5;
const MONO: &str = "Cascadia Mono";
const MONO_TEXT: f32 = 12.0;
/// Высота строки фрагмента: на эту высоту — подсветка совпадения.
const MONO_LINE: f32 = 14.0;
/// Значок флажка «Подкаталоги».
const CHECK_GLYPH: f32 = 13.0;
/// Кадр, пока идёт поиск: строки, счётчики, бегущий отрезок.
const TICK: Duration = Duration::from_millis(33);
/// Шапка предпросмотра.
const H_PREVIEW_HEAD: f32 = 30.0;

/// Поле формы.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Slot {
    Name,
    Text,
    Place,
}

impl Slot {
    const ALL: [Slot; 3] = [Slot::Name, Slot::Text, Slot::Place];

    fn idx(self) -> usize {
        self as usize
    }

    /// История поля (№70).
    fn kind(self) -> FieldKind {
        match self {
            Slot::Name => FieldKind::Mask,
            Slot::Text => FieldKind::Find,
            Slot::Place => FieldKind::Path,
        }
    }
}

/// Где клавиатура.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Zone {
    Form(Slot),
    List,
}

/// Tab: Имя → Текст → В → список → Имя.
fn next_zone(z: Zone, back: bool) -> Zone {
    const ORDER: [Zone; 4] = [
        Zone::Form(Slot::Name),
        Zone::Form(Slot::Text),
        Zone::Form(Slot::Place),
        Zone::List,
    ];
    let at = ORDER.iter().position(|o| *o == z).unwrap_or(0);
    let step = if back { ORDER.len() - 1 } else { 1 };
    ORDER[(at + step) % ORDER.len()]
}

/// Флажок формы.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Flag {
    Case,
    Regex,
    Recursive,
}

impl Flag {
    /// Клавиша флажка: Alt+буква, на любой раскладке. Первые две — как в «Найти» (S7).
    const KEYS: [(Key, Flag); 3] = [
        (Key::C, Flag::Case),
        (Key::R, Flag::Regex),
        (Key::S, Flag::Recursive),
    ];
}

/// Клавиша правки текста: идёт полю формы мимо keymap. Остальное (F3, Esc, Ctrl+Enter, ↑↓,
/// `PgUp`, `PgDn`) остаётся привязками экрана.
fn field_key(ks: &Keystroke) -> bool {
    let m = ks.modifiers;
    match ks.key.as_str() {
        "left" | "right" | "home" | "end" | "backspace" | "delete" => !m.alt,
        "insert" => m.shift || m.control,
        _ if m.control && !m.alt => matches!(
            super::input::stroke_key(ks),
            Some(Key::A | Key::C | Key::V | Key::X | Key::Y | Key::Z)
        ),
        _ if m.alt && !m.control => false,
        _ => ks
            .key_char
            .as_deref()
            .is_some_and(|t| super::input::typed(m, t)),
    }
}

/// Поле формы: `InputState` и где оно на экране.
struct FormField {
    input: Entity<InputState>,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    _sub: Subscription,
}

impl FormField {
    fn new(slot: Slot, init: &str, window: &mut Window, cx: &mut Context<CoreHost>) -> FormField {
        let init = init.to_owned();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(init));
        let sub = cx.subscribe_in(
            &input,
            window,
            move |h: &mut CoreHost, input, ev: &InputEvent, window, cx| match ev {
                // Клик в поле: клавиши — ему.
                InputEvent::Focus => {
                    if let Some(sc) = h
                        .features
                        .get_mut::<Search>()
                        .and_then(|me| me.screen.as_mut())
                    {
                        sc.zone = Zone::Form(slot);
                        sc.slot = slot;
                    }
                    cx.notify();
                }
                // Открытый список дополнений следует за набором (№70).
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    if let Some(c) = h.completion() {
                        let cmd = Command::new("ui.complete")
                            .arg("kind", c.kind.as_str())
                            .arg("text", text.as_str());
                        h.run(cmd, window, cx);
                    }
                }
                _ => {}
            },
        );
        FormField {
            input,
            bounds: Rc::new(Cell::new(None)),
            _sub: sub,
        }
    }

    fn text(&self, cx: &App) -> compact_str::CompactString {
        self.input.read(cx).value().to_string().into()
    }
}

/// Подписи keybar: поиск клавиш в раскладке — не на каждый кадр.
#[derive(Default)]
struct KeysCache {
    /// Поколение раскладки, язык, форма или список.
    key: Option<(u64, u64, bool)>,
    keys: Vec<(String, String)>,
}

impl KeysCache {
    fn get(&mut self, s: &AppState, form: bool) -> Vec<(String, String)> {
        let generation = s
            .ext
            .get::<urfin_keymap::ActiveKeymap>()
            .map_or(0, |a| a.generation);
        let key = Some((generation, urfin_i18n::epoch(), form));
        if self.key != key {
            self.key = key;
            self.keys = crate::keybar::search_keys(s, form);
        }
        self.keys.clone()
    }
}

/// Что сделать с фокусом в следующем кадре (там есть окно).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Want {
    /// Поле зоны с выделенным текстом, список — фокус окну.
    Enter,
    /// Вернуть фокус полю как есть: клик по флажку не уводит ввод.
    Keep,
}

/// Открытый экран.
struct Screen {
    form: Form,
    /// Поля живут в кадре: нужно окно.
    fields: Option<[FormField; 3]>,
    zone: Zone,
    /// Последнее поле формы: к нему возвращается Alt+F7.
    slot: Slot,
    want: Option<Want>,
    /// Фокус Core у экрана (прошлый кадр).
    active: bool,
    /// Что искал запущенный поиск: итог не меняется правкой формы.
    ran: Kinds,
    /// Стор на экране и когда он появился.
    store: Option<SearchId>,
    started: Instant,
    /// Первая видимая строка; курсор, за которым она следует.
    top: usize,
    cursor: Option<usize>,
    /// Дробные строки колеса (тачпад).
    wheel: f32,
    /// Площадь списка (прошлый кадр): окно строк и клик.
    list: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Строки списка после шейпинга (#345): их рисует `canvas`.
    lines: Rc<RefCell<RowCache>>,
    /// Форма — своя view (#345), её прошлый отпечаток и место (кадр из кэша — той же высоты).
    form_view: Option<Entity<FormView>>,
    form_print: u64,
    form_at: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Ключ перерисовки модели.
    rev: u64,
    keys: RefCell<KeysCache>,
    /// Предпросмотр: файл под курсором — через паузу.
    follow: Follow<Peek>,
    /// Документ предпросмотра этого кадра (рисует фича вьювера, [`preview`]).
    body: Option<AnyElement>,
}

impl Screen {
    fn new(form: Form, zone: Zone) -> Screen {
        let slot = match zone {
            Zone::Form(s) => s,
            Zone::List => Slot::Name,
        };
        Screen {
            ran: form.kinds,
            form,
            fields: None,
            zone,
            slot,
            want: Some(Want::Enter),
            active: false,
            store: None,
            started: Instant::now(),
            top: 0,
            cursor: None,
            wheel: 0.0,
            list: Rc::new(Cell::new(None)),
            lines: Rc::default(),
            form_view: None,
            form_print: 0,
            form_at: Rc::new(Cell::new(None)),
            rev: 0,
            keys: RefCell::default(),
            follow: Follow::default(),
            body: None,
        }
    }

    /// Перейти в зону; фокус — в кадре.
    fn go(&mut self, zone: Zone) {
        self.zone = zone;
        if let Zone::Form(s) = zone {
            self.slot = s;
        }
        self.want = Some(Want::Enter);
    }

    fn toggle(&mut self, flag: Flag) {
        let f = &mut self.form;
        match flag {
            Flag::Case => f.case = !f.case,
            Flag::Regex => f.regex = !f.regex,
            Flag::Recursive => f.recursive = !f.recursive,
        }
        if matches!(self.zone, Zone::Form(_)) {
            self.want = Some(Want::Keep);
        }
    }

    /// Enter в форме: текст полей — в форму, команды запуска, клавиатура — в список.
    fn run(&mut self, cx: &App) -> Vec<Command> {
        let Some(fields) = self.fields.as_ref() else {
            return Vec::new();
        };
        self.form.name = fields[Slot::Name.idx()].text(cx);
        self.form.text = fields[Slot::Text.idx()].text(cx);
        self.form.place = fields[Slot::Place.idx()].text(cx);
        self.ran = self.form.kinds;
        self.go(Zone::List);
        self.form.run()
    }

    fn fields(&mut self, window: &mut Window, cx: &mut Context<CoreHost>) {
        if self.fields.is_some() {
            return;
        }
        let form = &self.form;
        let inits = [form.name.as_str(), form.text.as_str(), form.place.as_str()];
        self.fields =
            Some(Slot::ALL.map(|slot| FormField::new(slot, inits[slot.idx()], window, cx)));
    }

    /// Просьба о фокусе из прошлого кадра.
    fn apply_focus(
        &mut self,
        want: Want,
        focus: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) {
        let Zone::Form(slot) = self.zone else {
            window.focus(focus, cx);
            return;
        };
        let Some(f) = self.fields.as_ref().map(|f| &f[slot.idx()]) else {
            return;
        };
        f.input.update(cx, |s, cx| {
            s.focus(window, cx);
            if want == Want::Enter {
                s.select_all(window, cx);
            }
        });
    }

    /// Стор сменился: новый поиск — с первой строки.
    fn sync_store(&mut self, store: Option<&Arc<SearchStore>>) {
        let id = store.map(|s| s.id);
        if self.store != id {
            self.store = id;
            self.top = 0;
            self.cursor = None;
            self.started = Instant::now();
        }
    }

    /// Строк в окне: целых и с неполной нижней.
    fn window_rows(&self, row_h: f32, fallback: f32) -> (usize, usize) {
        let h = self
            .list
            .get()
            .map_or(fallback, |b| f32::from(b.size.height));
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "строк на экране десятки, значение неотрицательно"
        )]
        let n = |v: f32| v.max(0.0) as usize;
        (n((h / row_h).floor()), n((h / row_h).ceil()))
    }

    fn element(&mut self, f: &Frame<'_>, window: &Window, cx: &Context<CoreHost>) -> AnyElement {
        let theme = f.theme;
        let m = f.s.ext.get::<SearchModel>();
        let store = m.and_then(SearchModel::current);
        self.sync_store(store);
        let row_h = theme.m(Metric::HRow);
        let (full, shown) = self.window_rows(row_h, f32::from(window.viewport_size().height));
        let len = store.map_or(0, |s| s.rows_len());
        let cursor = m.map_or(0, |m| m.cursor() as usize);
        self.top = if self.cursor == Some(cursor) {
            search_view::clamp_top(self.top, full, len)
        } else {
            search_view::follow(self.top, cursor, full, len)
        };
        self.cursor = Some(cursor);

        let rows = store.map_or_else(Default::default, |s| s.rows(self.top..self.top + shown));
        let (top, pal) = (self.top, Pal::new(theme));
        let (probe, row_x) = (Rc::clone(&self.list), theme.m(Metric::PadRowX));
        let (lines, store_id) = (Rc::clone(&self.lines), store.map(|s| s.id));
        let list = div()
            .id("search-list")
            .flex_1()
            .min_h_0()
            .child(
                canvas(
                    move |b, _, _| probe.set(Some(b)),
                    move |b, (), window, cx| {
                        let geo = Geo {
                            store: store_id,
                            top,
                            cursor,
                            row_h: px(row_h),
                            pad: px(row_x),
                        };
                        paint_rows(&rows, geo, pal, b, &mut lines.borrow_mut(), window, cx);
                    },
                )
                .size_full(),
            )
            .on_scroll_wheel(cx.listener(move |h, ev: &ScrollWheelEvent, _, cx| {
                if let Some(sc) = h
                    .features
                    .get_mut::<Search>()
                    .and_then(|me| me.screen.as_mut())
                {
                    sc.scroll(ev, row_h);
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |h, ev: &MouseDownEvent, w, cx| {
                    let cmds = h
                        .features
                        .get_mut::<Search>()
                        .and_then(|me| me.screen.as_mut())
                        .map_or_else(Vec::new, |sc| sc.click(ev, row_h, len));
                    for c in cmds {
                        h.run(c, w, cx);
                    }
                    cx.notify();
                }),
            );
        let left = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme.c(Role::StrokeDivider))
            .text_size(px(TEXT))
            .child(self.form(theme, cx))
            .child(head(theme))
            .child(list)
            .child(self.foot(m, store, theme));
        let body = self.body.take();
        let right = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .text_size(px(TEXT))
            .when_some(peek(f.s), |d, p| {
                d.child(preview_head(&p, theme)).children(body)
            });
        div()
            .size_full()
            .flex()
            .bg(theme.c(Role::LayerPanel))
            .text_color(theme.c(Role::TextPrimary))
            .child(left)
            .child(right)
            .into_any_element()
    }

    /// Курсор списка — цель предпросмотра; `Some(d)` — разбудить через `d`.
    fn follow_tick(&mut self, s: &AppState, now: Instant) -> Option<Duration> {
        let target = peek(s).filter(|_| on_screen(s));
        let side = preview_side(s);
        let shown = target.as_ref().filter(|p| {
            self.follow.asked() == Some(*p) && side.is_some_and(|side| shows(s, side, p))
        });
        self.follow.observe(target.as_ref(), shown, now)
    }

    fn scroll(&mut self, ev: &ScrollWheelEvent, row_h: f32) {
        self.wheel += f32::from(ev.delta.pixel_delta(px(row_h)).y);
        let steps = (self.wheel / row_h).trunc();
        self.wheel -= steps * row_h;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "шаг колеса мал"
        )]
        let n = steps.abs() as usize;
        self.top = if steps > 0.0 {
            self.top.saturating_sub(n)
        } else {
            self.top.saturating_add(n)
        };
    }

    /// Клик в списке: курсор на строку, двойной — переход.
    fn click(&mut self, ev: &MouseDownEvent, row_h: f32, len: usize) -> Vec<Command> {
        self.go(Zone::List);
        let Some(b) = self.list.get() else {
            return Vec::new();
        };
        let y = f32::from(ev.position.y - b.origin.y);
        let Some(row) = search_view::row_at(y, row_h, self.top, len) else {
            return Vec::new();
        };
        let mut cmds =
            vec![Command::new("ui.search.row").arg("row", i64::try_from(row).unwrap_or(i64::MAX))];
        if ev.click_count >= 2 {
            cmds.push(Command::new("search.goto"));
        }
        cmds
    }

    fn input_el(&self, slot: Slot, theme: &Theme) -> Div {
        let focused = self.active && self.zone == Zone::Form(slot);
        let edge = theme.c(if focused {
            Role::AccentFill
        } else {
            Role::TextSecondary
        });
        let f = self.fields.as_ref().map(|f| &f[slot.idx()]);
        let body = div()
            .size_full()
            .flex()
            .items_center()
            .relative()
            .rounded(px(4.0))
            .border_1()
            .border_color(theme.c(if focused {
                Role::AccentFill
            } else {
                Role::StrokeDivider
            }))
            .bg(theme.c(Role::LayerPanel))
            .children(f.map(|f| {
                Input::new(&f.input)
                    .small()
                    .appearance(false)
                    .text_size(px(TEXT))
            }))
            .children(f.map(|f| super::field::probe(Rc::clone(&f.bounds))));
        // Нижняя кромка темнее боковых (`inputStyle` макета): рамку gpui рисует поверх детей,
        // поэтому кромка — соседка поля.
        div().h(px(H_FIELD)).min_w_0().relative().child(body).child(
            div()
                .absolute()
                .left(px(2.0))
                .right(px(2.0))
                .bottom_0()
                .h(px(1.0))
                .bg(edge),
        )
    }

    /// Форма в кадре: в списке она не меняется — из кэша gpui, как части панели (#345). Кадр
    /// из кэша раскладку формы не считает, поэтому её высота — с прошлого кадра.
    fn form(&mut self, theme: &Theme, cx: &Context<CoreHost>) -> AnyElement {
        let key = self.form_key(theme, cx);
        let height = self.form_at.get().map(|b| b.size.height);
        let reuse = height.filter(|_| key.is_some() && key == Some(self.form_print));
        let el = match (self.form_view.clone(), key.filter(|_| height.is_some())) {
            (Some(view), Some(print)) => super::cached(&view, print, &mut self.form_print),
            (Some(view), None) => {
                self.form_print = 0;
                view.into_any_element()
            }
            (None, _) => self
                .form_el(theme, &Listen(cx.weak_entity()))
                .size_full()
                .into_any_element(),
        };
        div()
            .flex_none()
            .relative()
            .flex()
            .flex_col()
            .when_some(reuse, gpui_kit::Styled::h)
            .child(el)
            .child(super::field::probe(Rc::clone(&self.form_at)))
            .into_any_element()
    }

    /// Отпечаток формы; `None` — в поле фокус (курсор, выделение), кэша нет.
    fn form_key(&self, theme: &Theme, cx: &App) -> Option<u64> {
        use std::hash::{Hash as _, Hasher as _};
        if !self.active || self.zone != Zone::List {
            return None;
        }
        let mut h = rustc_hash::FxHasher::default();
        let f = &self.form;
        (f.case, f.regex, f.recursive).hash(&mut h);
        (theme.generation(), urfin_i18n::epoch()).hash(&mut h);
        for field in self.fields.as_ref()? {
            field.input.read(cx).value().hash(&mut h);
        }
        Some(h.finish() | 1)
    }

    fn form_el(&self, theme: &Theme, l: &Listen) -> Div {
        let i18n = urfin_i18n::current();
        let label = |key: &str, w: Option<f32>| {
            div()
                .flex_none()
                .when_some(w, |d, w| d.w(px(w)))
                .text_color(theme.c(Role::TextSecondary))
                .text_size(px(LABEL_TEXT))
                .child(SharedString::from(i18n.get(key)))
        };
        let tip = |el: Stateful<Div>, key: &str, hint: &str| {
            super::view::tip_lines(el, theme, vec![(i18n.get(key), Some(hint.to_owned()))])
        };
        let f = &self.form;
        let first = div()
            .flex()
            .items_center()
            .gap(px(FORM_GAP))
            .child(label("ui-search-name", Some(W_LABEL)))
            .child(self.input_el(Slot::Name, theme).flex_none().w(px(W_NAME)))
            .child(label("ui-search-text", None))
            .child(self.input_el(Slot::Text, theme).flex_1())
            .child(tip(
                chip("search-case", "Aa", f.case, theme).on_click(flag_click(Flag::Case, l)),
                "ui-search-case-tip",
                "Alt+C",
            ))
            .child(tip(
                chip("search-regex", ".*", f.regex, theme).on_click(flag_click(Flag::Regex, l)),
                "ui-search-regex-tip",
                "Alt+R",
            ));
        let second = div()
            .flex()
            .items_center()
            .gap(px(FORM_GAP))
            .child(label("ui-search-where", Some(W_LABEL)))
            .child(self.input_el(Slot::Place, theme).flex_1())
            .child(recursive(f.recursive, theme, l));
        div()
            .flex_none()
            .px(px(theme.m(Metric::PadRowX)))
            .py(px(FORM_PAD_Y))
            .flex()
            .flex_col()
            .gap(px(FORM_GAP))
            .border_b_1()
            .border_color(theme.c(Role::StrokeDivider))
            .child(first)
            .child(second)
    }

    /// Низ: полоса 2 `accent` и итог.
    fn foot(
        &self,
        m: Option<&SearchModel>,
        store: Option<&Arc<SearchStore>>,
        theme: &Theme,
    ) -> Div {
        let done = store.is_some_and(|s| s.is_done());
        let ms = store.map_or(0, |s| {
            if done {
                s.info.elapsed_ms.load(Ordering::Acquire)
            } else {
                u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
            }
        });
        let (found, scanned) =
            m.map_or_else(Default::default, |m| search_view::summary(m, self.ran, ms));
        let bar = div()
            .h(px(2.0))
            .relative()
            .overflow_hidden()
            .when(done, |d| d.bg(theme.c(Role::AccentFill)))
            .when(store.is_some() && !done, |d| {
                d.child(super::du::sweep(theme).top_0())
            });
        div()
            .flex_none()
            .px(px(theme.m(Metric::PadRowX)))
            .py(px(8.0))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .border_t_1()
            .border_color(theme.c(Role::StrokeDivider))
            .child(bar)
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_size(px(LABEL_TEXT))
                    .text_color(theme.c(Role::TextSecondary))
                    .child(SharedString::from(found))
                    .child(SharedString::from(scanned)),
            )
    }
}

/// Цель предпросмотра: файл строки под курсором списка.
#[derive(Clone, Debug, PartialEq)]
struct Peek {
    store: SearchId,
    loc: Location,
    /// Строка с 1; 0 — поиск без текста: начало файла, без подсветки.
    line: u32,
    /// Текст совпадения из фрагмента.
    hit: Option<CompactString>,
    /// Шапка: папка корня поиска и путь от него (корней может быть несколько).
    title: String,
}

/// Строка под курсором списка, если это файл.
fn peek(s: &AppState) -> Option<Peek> {
    let m = s.ext.get::<SearchModel>()?;
    let store = m.current()?;
    let row = m.cursor() as usize;
    let hit = store.rows(row..row + 1).iter().next()?.clone();
    if hit.e.kind.is_dir_like() {
        return None;
    }
    let root = store.roots.get(usize::from(hit.root))?;
    let text = (hit.line > 0)
        .then(|| search_view::hit_parts(&hit).1)
        .filter(|t| !t.is_empty())
        .map(CompactString::from);
    let title = root.path.file_name().map_or_else(
        || root.path.join(hit.e.name.as_str()),
        |n| std::path::Path::new(n).join(hit.e.name.as_str()),
    );
    Some(Peek {
        store: store.id,
        loc: urfin_vfs::walk::child(root, &hit.e),
        line: hit.line,
        hit: text,
        title: title.to_string_lossy().into_owned(),
    })
}

/// Слот области панели под предпросмотр: справа, если там нет перенесённого вьювера
/// (`viewer.dock`), иначе слева.
fn preview_side(s: &AppState) -> Option<Side> {
    let v = s.ext.get::<ViewState>()?;
    [Side::Right, Side::Left]
        .into_iter()
        .find(|&side| v.in_panel(side).is_none_or(|d| d.quick))
}

/// Слот `side` показывает файл и строку `p` (или заметку о нём).
fn shows(s: &AppState, side: Side, want: &Peek) -> bool {
    let Some(view) = s.ext.get::<ViewState>() else {
        return false;
    };
    if !view.qpreview[side.idx()] {
        return false;
    }
    match view.in_panel(side) {
        Some(doc) => {
            doc.quick
                && doc.loc == want.loc
                && match (&doc.preview, want.line) {
                    (None, 0) => true,
                    (Some(at), line) => line > 0 && at.line + 1 == u64::from(line),
                    (None, _) => false,
                }
        }
        None => view.qnote[side.idx()]
            .as_ref()
            .is_some_and(|note| note.loc == want.loc),
    }
}

/// `viewer.quick` предпросмотра: без строки — начало файла, без подсветки.
fn peek_cmd(p: &Peek, side: Side) -> Command {
    let side = match side {
        Side::Left => "left",
        Side::Right => "right",
    };
    let mut c = Command::new("viewer.quick")
        .arg("side", side)
        .arg("path", p.loc.clone());
    if p.line > 0 {
        c = c.arg("line", p.line);
        if let Some(h) = &p.hit {
            c = c.arg("hit", h.as_str());
        }
    }
    c
}

/// Шапка предпросмотра: путь и «стр N · предпросмотр».
fn preview_head(p: &Peek, theme: &Theme) -> Div {
    let line = (p.line > 0).then(|| {
        let mut a = urfin_i18n::FluentArgs::new();
        a.set("line", p.line);
        urfin_i18n::current().fmt("ui-search-preview-line", &a)
    });
    div()
        .flex_none()
        .h(px(H_PREVIEW_HEAD))
        .px(px(theme.m(Metric::PadRowX)))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.0))
        .border_b_1()
        .border_color(theme.c(Role::StrokeDivider))
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(SharedString::from(p.title.clone())),
        )
        .children(line.map(|l| {
            div()
                .flex_none()
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(l))
        }))
}

/// Сцена `--screens` готова к снимку: предпросмотр показывает строку под курсором.
pub(super) fn settled(s: &AppState) -> bool {
    let (Some(p), Some(side)) = (peek(s), preview_side(s)) else {
        return true;
    };
    !on_screen(s) || (shows(s, side, &p) && super::view::preview_settled(s, side))
}

/// Документ предпросмотра — экрану поиска, до его `main`: рисует фича вьювера (её кэш
/// строк, мышь, геометрия `Viewport`).
pub(super) fn preview(
    features: &mut Features,
    f: &Frame<'_>,
    window: &mut Window,
    cx: &Context<CoreHost>,
) {
    let open = features
        .get::<Search>()
        .is_some_and(|me| me.screen.is_some());
    if !open || super::view::screen_shown(f.s) || peek(f.s).is_none() {
        return;
    }
    let body =
        preview_side(f.s).and_then(|side| super::view::search_body(features, f, side, window, cx));
    if let Some(sc) = features
        .get_mut::<Search>()
        .and_then(|me| me.screen.as_mut())
    {
        sc.body = body;
    }
}

/// Шапка списка.
fn head(theme: &Theme) -> Div {
    let i18n = urfin_i18n::current();
    div()
        .flex_none()
        .h(px(H_HEAD))
        .px(px(theme.m(Metric::PadRowX)))
        .flex()
        .items_center()
        .border_b_1()
        .border_color(theme.c(Role::StrokeDivider))
        .text_size(px(HEAD_TEXT))
        .text_color(theme.c(Role::TextSecondary))
        .child(
            div()
                .w(px(W_FILE))
                .child(SharedString::from(i18n.get("ui-search-col-file"))),
        )
        .child(
            div()
                .flex_1()
                .child(SharedString::from(i18n.get("ui-search-col-match"))),
        )
}

/// «Подкаталоги»: флажок как на макете, значок крупнее подписи.
fn recursive(on: bool, theme: &Theme, l: &Listen) -> Stateful<Div> {
    let tip = urfin_i18n::current().get("ui-search-recursive");
    let glyph = if on { "\u{E73D}" } else { "\u{E739}" };
    super::view::tip_lines(
        div()
            .id("search-recursive")
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.0))
            .text_size(px(LABEL_TEXT))
            .whitespace_nowrap()
            .child(
                div()
                    .font_family("Segoe Fluent Icons")
                    .text_size(px(CHECK_GLYPH))
                    .text_color(theme.c(if on {
                        Role::AccentText
                    } else {
                        Role::TextSecondary
                    }))
                    .child(glyph),
            )
            .child(SharedString::from(tip.clone()))
            .on_click(flag_click(Flag::Recursive, l)),
        theme,
        vec![(tip, Some("Alt+S".to_owned()))],
    )
}

/// Чип формы: включён — рамка `accent` на `cursor`.
fn chip(id: &'static str, text: &'static str, on: bool, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(H_CHIP))
        .px(px(10.0))
        .flex()
        .items_center()
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.c(if on {
            Role::AccentFill
        } else {
            Role::StrokeDivider
        }))
        .text_size(px(LABEL_TEXT))
        .text_color(theme.c(if on {
            Role::CursorText
        } else {
            Role::TextPrimary
        }))
        .when(on, |d| d.bg(theme.c(Role::FillCursor)))
        .child(text)
}

fn flag_click(flag: Flag, l: &Listen) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    l.listener(move |h, _: &ClickEvent, _, cx| {
        if let Some(sc) = h
            .features
            .get_mut::<Search>()
            .and_then(|me| me.screen.as_mut())
        {
            sc.toggle(flag);
            cx.notify();
        }
    })
}

/// Форма экрана — своя view: окно кладёт её кэшированной ([`Screen::form`]).
pub(super) struct FormView {
    host: WeakEntity<CoreHost>,
}

impl Render for FormView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(host) = self.host.upgrade() else {
            return div();
        };
        let h = host.read(cx);
        let Some(sc) = h.features.get::<Search>().and_then(|me| me.screen.as_ref()) else {
            return div();
        };
        // Кадр из кэша кладёт форму корнем в место прошлого кадра: на всю ширину и высоту.
        sc.form_el(&h.theme, &Listen(self.host.clone())).size_full()
    }
}

/// Цвета строк.
#[derive(Copy, Clone)]
struct Pal {
    text: Hsla,
    secondary: Hsla,
    cursor: Hsla,
    cursor_text: Hsla,
    highlight: Hsla,
}

/// Для кэша строк — цвета текста: заливки курсора и совпадения рисуются каждый кадр.
impl PartialEq for Pal {
    fn eq(&self, o: &Pal) -> bool {
        [self.text, self.secondary, self.cursor_text] == [o.text, o.secondary, o.cursor_text]
    }
}

impl Pal {
    fn new(theme: &Theme) -> Pal {
        Pal {
            text: theme.c(Role::TextPrimary),
            secondary: theme.c(Role::TextSecondary),
            cursor: theme.c(Role::FillCursor),
            cursor_text: theme.c(Role::CursorText),
            highlight: theme.c(Role::Highlight),
        }
    }
}

/// Геометрия окна строк.
#[derive(Copy, Clone)]
struct Geo {
    store: Option<SearchId>,
    /// Номер первой строки окна.
    top: usize,
    cursor: usize,
    row_h: Pixels,
    /// Поле слева и справа.
    pad: Pixels,
}

fn run(len: usize, font: &Font, color: Hsla) -> TextRun {
    TextRun {
        len,
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// Строка в ширину `room`: не влезает — с «…». Вторым — длина текста до «…».
fn fit(
    text: SharedString,
    font: &Font,
    color: Hsla,
    px_size: Pixels,
    room: Pixels,
    window: &mut Window,
    cx: &App,
) -> (gpui_kit::ShapedLine, usize) {
    let runs = [run(text.len(), font, color)];
    let line = window
        .text_system()
        .shape_line(text.clone(), px_size, &runs, None);
    if line.width <= room {
        return (line, text.len());
    }
    let (cut, runs) = cx
        .text_system()
        .line_wrapper(font.clone(), px_size)
        .truncate_line(text, room, "…", &runs, TruncateFrom::End);
    let kept = cut.len().saturating_sub("…".len());
    (
        window.text_system().shape_line(cut, px_size, &runs, None),
        kept,
    )
}

/// [`fit`] для моноширинного шрифта: ширина строки — сумма ширин знаков, та же, что считает
/// обрезка, поэтому сначала обрезка, затем один шейпинг DirectWrite вместо двух (#345).
fn fit_mono(
    text: SharedString,
    font: &Font,
    color: Hsla,
    px_size: Pixels,
    room: Pixels,
    window: &mut Window,
    cx: &App,
) -> (gpui_kit::ShapedLine, usize) {
    let runs = [run(text.len(), font, color)];
    let (cut, runs) = cx
        .text_system()
        .line_wrapper(font.clone(), px_size)
        .truncate_line(text, room, "…", &runs, TruncateFrom::End);
    // Не обрезана — прогоны те же (`Cow::Borrowed`).
    let kept = match runs {
        std::borrow::Cow::Borrowed(_) => cut.len(),
        std::borrow::Cow::Owned(_) => cut.len().saturating_sub("…".len()),
    };
    (
        window.text_system().shape_line(cut, px_size, &runs, None),
        kept,
    )
}

/// Строка списка после шейпинга и обрезки.
struct RowLines {
    name: gpui_kit::ShapedLine,
    line_no: Option<gpui_kit::ShapedLine>,
    /// Фрагмент и длина его текста до «…».
    preview: Option<(gpui_kit::ShapedLine, usize)>,
}

/// Что меняет все строки сразу: стор, ширина списка, шрифт, цвета.
#[derive(PartialEq)]
struct RowKey {
    store: Option<SearchId>,
    width: Pixels,
    font: Font,
    pal: Pal,
}

/// Строка списка без курсора и под курсором: цвета разные, шейпинг — по требованию. В куче:
/// `ShapedLine` велик (прогоны на стеке), таблица его не копирует.
type RowPair = [Option<Box<RowLines>>; 2];

/// Кэш строк списка (#345): строка стора не меняется (стор только дописывается), поэтому
/// строка, что была на экране в прошлом кадре, не шейпится и не обрезается заново. Ключ —
/// номер строки. Номера `:N` — свой кэш: на новой странице они повторяются.
#[derive(Default)]
struct RowCache {
    key: Option<RowKey>,
    rows: FxHashMap<usize, RowPair>,
    /// Таблица прошлого кадра, пустая: память на следующий кадр.
    spare: FxHashMap<usize, RowPair>,
    numbers: FxHashMap<(u32, bool), gpui_kit::ShapedLine>,
}

/// Номеров `:N` в кэше; больше — кэш начинается заново (`ShapedLine` ≈ 3 КБ: до 3 МБ).
const NUMBERS_MAX: usize = 1024;

impl RowCache {
    /// Начало кадра: строки прошлого кадра; сменился ключ — пусто.
    fn frame(&mut self, key: RowKey) -> FxHashMap<usize, RowPair> {
        if self.key.as_ref() != Some(&key) {
            self.key = Some(key);
            self.rows.clear();
            self.numbers.clear();
        }
        if self.numbers.len() > NUMBERS_MAX {
            self.numbers.clear();
        }
        let spare = std::mem::take(&mut self.spare);
        std::mem::replace(&mut self.rows, spare)
    }

    /// Конец кадра: строки, ушедшие с экрана, — прочь.
    fn done(&mut self, mut prev: FxHashMap<usize, RowPair>) {
        prev.clear();
        self.spare = prev;
    }
}

/// Шрифты и место кадра для [`shape_row`].
struct RowFonts<'a> {
    font: &'a Font,
    mono: &'a Font,
    /// Ширина фрагмента.
    room: Pixels,
}

/// Шейпинг строки `h`: файл (номер строки виден всегда, обрезается имя) и фрагмент.
fn shape_row(
    h: &Hit,
    on: bool,
    pal: Pal,
    f: &RowFonts<'_>,
    numbers: &mut FxHashMap<(u32, bool), gpui_kit::ShapedLine>,
    window: &mut Window,
    cx: &App,
) -> RowLines {
    let fg = if on { pal.cursor_text } else { pal.text };
    let line_no = (h.line > 0).then(|| {
        numbers
            .entry((h.line, on))
            .or_insert_with(|| {
                let t = SharedString::from(format!(":{}", h.line));
                let color = if on { fg } else { pal.secondary };
                let runs = [run(t.len(), f.font, color)];
                window
                    .text_system()
                    .shape_line(t, px(ROW_TEXT), &runs, None)
            })
            .clone()
    });
    let tail = line_no.as_ref().map_or(px(0.0), |l| l.width);
    let name = SharedString::from(h.e.name.to_string());
    let (name, _) = fit(
        name,
        f.font,
        fg,
        px(ROW_TEXT),
        px(W_FILE) - tail,
        window,
        cx,
    );
    let text = h.preview.as_deref().unwrap_or_default();
    let preview = (!text.is_empty()).then(|| {
        let text = SharedString::from(text.to_owned());
        fit_mono(text, f.mono, fg, px(MONO_TEXT), f.room, window, cx)
    });
    RowLines {
        name,
        line_no,
        preview,
    }
}

/// Строки одним `canvas`: `файл:строка` и фрагмент моно, совпадение — `highlight`. Клик и
/// колесо — у контейнера, строкам элементы не нужны: кадр при 1M строк читает только окно.
fn paint_rows(
    rows: &search::Window<Hit>,
    g: Geo,
    pal: Pal,
    b: Bounds<Pixels>,
    cache: &mut RowCache,
    window: &mut Window,
    cx: &mut App,
) {
    let font = window.text_style().font();
    let mono = gpui_kit::font(MONO);
    let (x0, right) = (b.origin.x + g.pad, b.origin.x + b.size.width - g.pad);
    let hit_x = x0 + px(W_FILE + ROW_GAP);
    let fonts = RowFonts {
        font: &font,
        mono: &mono,
        room: (right - hit_x).max(px(0.0)),
    };
    let mut prev = cache.frame(RowKey {
        store: g.store,
        width: b.size.width,
        font: font.clone(),
        pal,
    });
    window.with_content_mask(Some(ContentMask { bounds: b }), |window| {
        let mut y = b.origin.y;
        for (k, h) in rows.iter().enumerate() {
            let on = g.top + k == g.cursor;
            if on {
                let row = Bounds::new(point(b.origin.x, y), size(b.size.width, g.row_h));
                window.paint_quad(gpui_kit::fill(row, pal.cursor));
            }
            let mut pair = prev.remove(&(g.top + k)).unwrap_or_default();
            let lines = pair[usize::from(on)].get_or_insert_with(|| {
                Box::new(shape_row(
                    h,
                    on,
                    pal,
                    &fonts,
                    &mut cache.numbers,
                    window,
                    cx,
                ))
            });
            let name = &lines.name;
            let _ = name.paint(point(x0, y), g.row_h, TextAlign::Left, None, window, cx);
            if let Some(l) = &lines.line_no {
                let at = point(x0 + name.width, y);
                let _ = l.paint(at, g.row_h, TextAlign::Left, None, window, cx);
            }
            // Фрагмент: подсветка под текстом, в полную высоту строки моно.
            if let Some((line, kept)) = &lines.preview {
                let (pre, hit, _) = search_view::hit_parts(h);
                if !hit.is_empty() && pre.len() < *kept {
                    let from = line.x_for_index(pre.len());
                    let to = line.x_for_index((pre.len() + hit.len()).min(*kept));
                    let top = y + (g.row_h - px(MONO_LINE)) / 2.0;
                    let area =
                        Bounds::new(point(hit_x + from, top), size(to - from, px(MONO_LINE)));
                    window.paint_quad(gpui_kit::fill(area, pal.highlight).corner_radii(px(2.0)));
                }
                let _ = line.paint(point(hit_x, y), g.row_h, TextAlign::Left, None, window, cx);
            }
            cache.rows.insert(g.top + k, pair);
            y += g.row_h;
        }
    });
    cache.done(prev);
}

/// Фича окна: экран поиска.
#[derive(Default)]
pub(super) struct Search {
    screen: Option<Screen>,
    /// Экран закрыт: вернуть фокус окну в следующем кадре.
    blur: bool,
    /// Замер `--bench-search`.
    bench: Option<SearchBench>,
}

fn on_screen(s: &AppState) -> bool {
    s.focus == Focus::Overlay(OverlayKind::Search)
}

impl Search {
    /// Ширины наборов табов заголовка: экран открыт — один набор активной панели на
    /// всё окно, как у вьювера (S8 §2.5, `screen-search`); иначе — по панелям.
    /// `win_w` — ширина окна; панели скрыты — табов нет, как обычно.
    pub(super) fn tab_widths(
        &self,
        s: &AppState,
        widths: [Option<f32>; 2],
        win_w: f32,
    ) -> [Option<f32>; 2] {
        let one = self.screen.is_some() && !super::view::screen_shown(s);
        if !one || widths == [None, None] {
            return widths;
        }
        match s.active {
            Side::Left => [Some(win_w), None],
            Side::Right => [None, Some(win_w)],
        }
    }

    /// Статус и клавиши экрана (`screen-search`). `None` — документ поверх экрана.
    pub(super) fn bottom(&self, s: &AppState) -> Option<SshBottom> {
        let sc = self.screen.as_ref()?;
        if super::view::screen_shown(s) {
            return None;
        }
        let m = s.ext.get::<SearchModel>()?;
        let list = s
            .ext
            .get::<crate::UiState>()
            .is_some_and(|u| u.complete.as_ref().is_some_and(|c| !c.items.is_empty()));
        let form = matches!(sc.zone, Zone::Form(_));
        let keys = (on_screen(s) && !list).then(|| sc.keys.borrow_mut().get(s, form));
        Some((Some(search_view::status(m, sc.ran)), keys))
    }

    /// Шаг замера `--bench-search`: команды вместо ввода.
    fn bench_step(&mut self, s: &AppState, window: &mut Window, cx: &mut Context<CoreHost>) {
        if !crate::bench::SEARCH.load(Ordering::Relaxed) {
            return;
        }
        window.request_animation_frame();
        let Some(b) = self.bench.as_mut() else {
            if let Some(open) = crate::bench::open_due() {
                self.bench = Some(SearchBench::new());
                send(open, window, cx);
            }
            return;
        };
        let rows = s
            .ext
            .get::<SearchModel>()
            .and_then(SearchModel::current)
            .map_or(0, |st| st.rows_len());
        match b.frame(rows) {
            Act::None => {}
            Act::Cursor(to) => send(Command::new("search.cursor").arg("to", to), window, cx),
            Act::Exit(report) => {
                use std::io::Write as _;
                println!("bench search {report}");
                let _ = std::io::stdout().flush();
                std::process::exit(0);
            }
        }
    }
}

/// Команда замера: проходом хоста после кадра.
fn send(c: Command, window: &mut Window, cx: &mut Context<CoreHost>) {
    cx.defer_in(window, move |h, w, cx| {
        if let Some(core) = h.core.as_mut() {
            core.dispatch(c);
        }
        h.pump(w, cx);
    });
}

impl ShellFeature for Search {
    /// `search.screen`: открыть экран или закрыть (`close`); открытый экран — к форме.
    fn ext(
        &mut self,
        id: &str,
        args: &Args,
        _s: &AppState,
        cx: &mut Context<CoreHost>,
    ) -> Option<Vec<Command>> {
        // Панель результатов (S8-11): запрос — в `UiState`, его читают заголовок и вкладка.
        if id == "search.panel" {
            let mut c = Command::new("ui.search.label");
            c.args = args.clone();
            return Some(vec![c]);
        }
        if id != "search.screen" {
            return None;
        }
        cx.notify();
        if args.bool("close") == Some(true) {
            self.screen = None;
            self.blur = true;
            return Some(vec![Command::new("ui.search.focus").arg("open", false)]);
        }
        let form = Form::from_args(args);
        if let Some(sc) = self.screen.as_mut() {
            sc.form.kinds = form.kinds;
            sc.go(Zone::Form(sc.slot));
        } else {
            // Сцены: снимок со строкой в списке или с полем «Текст» в фокусе.
            let zone = match args.str("zone") {
                Some("list") => Zone::List,
                Some("text") => Zone::Form(Slot::Text),
                _ => Zone::Form(Slot::Name),
            };
            self.screen = Some(Screen::new(form, zone));
        }
        Some(vec![Command::new("ui.search.focus")])
    }

    /// Пока стор растёт — кадр 30 раз в секунду; курсор встал — предпросмотр через паузу.
    /// Панель результатов, пока её поиск идёт, — тоже 30 раз в секунду (прогресс, счётчики).
    fn tick(&mut self, s: &AppState, now: Instant) -> Option<Duration> {
        let panel = [Side::Left, Side::Right]
            .into_iter()
            .any(|side| crate::search_panel::running(s, s.panel(side).tab()))
            .then_some(TICK);
        let Some(sc) = self.screen.as_mut() else {
            return panel;
        };
        let peek = sc.follow_tick(s, now);
        let m = s.ext.get::<SearchModel>()?;
        let rev = m.rev();
        let changed = std::mem::replace(&mut sc.rev, rev) != rev;
        let running = m.current().is_some_and(|st| !st.is_done());
        let frame = (changed || running).then_some(TICK).or(panel);
        match (frame, peek) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Документ закрыт, вопрос отвечен: Core вернул фокус панели, а на месте панелей экран.
    /// Курсор простоял паузу — `viewer.quick` предпросмотра.
    fn due(&mut self, s: &AppState, now: Instant) -> Vec<Command> {
        let Some(sc) = self.screen.as_mut() else {
            return Vec::new();
        };
        if s.focus == Focus::Panel {
            return vec![Command::new("ui.search.focus")];
        }
        sc.follow_tick(s, now);
        match (sc.follow.due(now), preview_side(s)) {
            (Some(p), Some(side)) if on_screen(s) => vec![peek_cmd(&p, side)],
            _ => Vec::new(),
        }
    }

    /// Tab — по зонам, Enter — запуск или переход, Alt+буква — флажки формы.
    fn key_in(
        &mut self,
        s: &AppState,
        ks: &Keystroke,
        _window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) -> Option<Vec<Command>> {
        if !on_screen(s) {
            return None;
        }
        let sc = self.screen.as_mut()?;
        let m = ks.modifiers;
        match ks.key.as_str() {
            "tab" if !m.control && !m.alt => {
                sc.go(next_zone(sc.zone, m.shift));
                return Some(Vec::new());
            }
            "enter" if !m.control && !m.alt => {
                return Some(match sc.zone {
                    Zone::Form(_) => sc.run(cx),
                    Zone::List => vec![Command::new("search.goto")],
                });
            }
            _ => {}
        }
        if m.alt && !m.control && !m.platform {
            let k = super::input::stroke_key(ks)?;
            let (_, flag) = Flag::KEYS.iter().find(|(key, _)| *key == k)?;
            sc.toggle(*flag);
            return Some(Vec::new());
        }
        None
    }

    fn editing_key(&self, s: &AppState, ks: &Keystroke) -> bool {
        on_screen(s)
            && self
                .screen
                .as_ref()
                .is_some_and(|sc| matches!(sc.zone, Zone::Form(_)))
            && field_key(ks)
    }

    fn field(&self, s: &AppState) -> Option<FeatureField> {
        let sc = self.screen.as_ref().filter(|_| on_screen(s))?;
        let Zone::Form(slot) = sc.zone else {
            return None;
        };
        let f = &sc.fields.as_ref()?[slot.idx()];
        Some(FeatureField {
            kind: slot.kind(),
            input: f.input.clone(),
            bounds: Rc::clone(&f.bounds),
        })
    }

    /// Поля формы и фокус: поверх экрана документ или меню — ввод не остаётся в поле.
    fn input(
        &mut self,
        s: &AppState,
        focus: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) {
        self.bench_step(s, window, cx);
        if std::mem::take(&mut self.blur) {
            window.focus(focus, cx);
        }
        let Some(sc) = self.screen.as_mut() else {
            return;
        };
        sc.fields(window, cx);
        if sc.form_view.is_none() {
            let host = cx.weak_entity();
            sc.form_view = Some(cx.new(|_| FormView { host }));
        }
        let active = on_screen(s);
        if active != std::mem::replace(&mut sc.active, active) {
            if active {
                sc.want = sc.want.or(Some(Want::Keep));
            } else {
                window.focus(focus, cx);
            }
        }
        if active && let Some(want) = sc.want.take() {
            sc.apply_focus(want, focus, window, cx);
        }
    }

    fn main(
        &mut self,
        f: &Frame<'_>,
        _hidden: bool,
        window: &mut Window,
        cx: &Context<CoreHost>,
    ) -> Option<Main> {
        let sc = self.screen.as_mut()?;
        if super::view::screen_shown(f.s) {
            return None;
        }
        Some(Main::Replace(sc.element(f, window, cx)))
    }
}

/// Сцены на фикстуре `screens.search`: не зависят от диска. Последняя закрывает экран.
pub(super) const SCENES: &[Scene] = &[
    Scene {
        name: "screen-search",
        steps: &[
            "ui.cancel",
            "resize 1280x760",
            "panel.focus side=left",
            "screens.search",
        ],
    },
    Scene {
        name: "search-names",
        steps: &["screens.search kind=names"],
    },
    Scene {
        name: "search-dirs",
        steps: &["screens.search kind=dirs"],
    },
    Scene {
        name: "search-form",
        steps: &["screens.search kind=form"],
    },
    // Настоящий запуск по демо-дереву: Alt+F7 и Enter — командами, обход и строки живые.
    Scene {
        name: "search-live",
        steps: &[
            "search.close",
            "search.files",
            "search.run name=*.toml",
            super::screens::WAIT_JOBS,
        ],
    },
    Scene {
        name: "search-live-text",
        steps: &[
            "search.run name=*.toml text=version",
            super::screens::WAIT_JOBS,
        ],
    },
    Scene {
        name: "search-closed",
        steps: &["search.close"],
    },
    // Панель результатов (S8-11) на фикстуре: поиск по имени идёт; отметка и курсор —
    // следующей сценой, когда строки в листинге.
    Scene {
        name: "searchpanel-plain",
        steps: &[
            "resize 1280x760",
            "panel.focus side=left",
            "screens.search kind=panel",
        ],
    },
    Scene {
        name: "screen-searchpanel",
        steps: &[
            r"panel.cursor.to name=urfin\crates\urfin-core\Cargo.toml",
            "sel.toggle",
            r"panel.cursor.to name=urfin\crates\urfin-ops\Cargo.toml",
            "sel.toggle",
            r"panel.cursor.to name=urfin\crates\urfin-vfs\Cargo.toml",
            "sel.toggle",
            r"panel.cursor.to name=urfin\crates\urfin-search\Cargo.toml",
        ],
    },
    // Поиск по тексту готов: колонки «Стр» и «Первое совпадение»; отметок на макете нет.
    Scene {
        name: "searchgrep-plain",
        steps: &["tab.close", "screens.search kind=grep"],
    },
    Scene {
        name: "screen-searchgrep",
        steps: &[r"panel.cursor.to name=urfin-search\src\svfs.rs"],
    },
    Scene {
        name: "searchpanel-closed",
        steps: &["tab.close"],
    },
];

#[cfg(test)]
mod tests {
    use gpui_kit::Modifiers;

    use super::*;

    fn ks(key: &str, ch: Option<&str>, m: Modifiers) -> Keystroke {
        Keystroke {
            modifiers: m,
            key: key.to_owned(),
            key_char: ch.map(str::to_owned),
        }
    }

    fn plain(key: &str, ch: Option<&str>) -> Keystroke {
        ks(key, ch, Modifiers::default())
    }

    #[test]
    fn text_keys_go_to_the_field() {
        for key in ["left", "right", "home", "end", "backspace", "delete"] {
            assert!(field_key(&plain(key, None)), "{key}");
        }
        assert!(field_key(&plain("a", Some("a"))));
        assert!(field_key(&plain("ф", Some("ф"))));
        assert!(field_key(&plain("space", Some(" "))));
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        assert!(field_key(&ks("left", None, shift)), "выделение");
        assert!(field_key(&ks("insert", None, shift)), "вставка");
        let ctrl = Modifiers {
            control: true,
            ..Modifiers::default()
        };
        for key in ["a", "c", "v", "x", "y", "z"] {
            assert!(field_key(&ks(key, None, ctrl)), "Ctrl+{key}");
        }
    }

    #[test]
    fn screen_keys_stay_with_keymap() {
        for key in ["f3", "f4", "escape", "up", "down", "pageup", "pagedown"] {
            assert!(!field_key(&plain(key, None)), "{key}");
        }
        assert!(!field_key(&plain("enter", Some("\n"))));
        let ctrl = Modifiers {
            control: true,
            ..Modifiers::default()
        };
        assert!(!field_key(&ks("enter", None, ctrl)), "Ctrl+Enter");
        assert!(!field_key(&ks("o", None, ctrl)), "Ctrl+O");
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        assert!(!field_key(&ks("f7", None, alt)), "Alt+F7");
        assert!(!field_key(&ks("c", Some("c"), alt)), "Alt+C");
    }

    #[test]
    fn altgr_types_into_the_field() {
        let altgr = Modifiers {
            control: true,
            alt: true,
            ..Modifiers::default()
        };
        assert!(field_key(&ks("e", Some("€"), altgr)));
    }

    #[test]
    fn tab_walks_form_and_list() {
        let name = Zone::Form(Slot::Name);
        let text = Zone::Form(Slot::Text);
        let place = Zone::Form(Slot::Place);
        assert_eq!(next_zone(name, false), text);
        assert_eq!(next_zone(text, false), place);
        assert_eq!(next_zone(place, false), Zone::List);
        assert_eq!(next_zone(Zone::List, false), name);
        assert_eq!(next_zone(name, true), Zone::List);
        assert_eq!(next_zone(Zone::List, true), place);
    }

    fn state_with(hits: Vec<Hit>) -> AppState {
        let tab =
            |id| urfin_core::Tab::new(urfin_core::TabId(id), Location::local(r"C:\Dev\urfin"));
        let mut s = AppState::new(
            urfin_core::Panel::new(tab(1)),
            urfin_core::Panel::new(tab(2)),
        );
        let mut m = SearchModel::default();
        let store = m.open(vec![Location::local(r"C:\Dev\urfin\crates")]);
        store.append(hits);
        m.show(Some(store.id));
        s.ext.insert(m);
        s
    }

    fn hit(name: &str, kind: urfin_core::EntryKind, line: u32, preview: &str) -> Hit {
        let at = preview.find("spawn").unwrap_or(0);
        Hit {
            e: urfin_core::Entry {
                name: name.into(),
                size: 1,
                mtime: 0,
                ctime: 0,
                atime: 0,
                attrs: urfin_core::Attrs::default(),
                kind,
            },
            root: 0,
            line,
            hl: (
                u16::try_from(at).unwrap_or(0),
                u16::try_from(at + 5).unwrap_or(0),
            ),
            preview: (!preview.is_empty()).then(|| preview.into()),
            status: 0,
        }
    }

    #[test]
    fn preview_follows_the_cursor_row() {
        let file = urfin_core::EntryKind::File;
        let s = state_with(vec![
            hit(
                r"urfin-core\src\ops\copy.rs",
                file,
                142,
                "pool.spawn_blocking(",
            ),
            hit(r"urfin-core\src\lib.rs", file, 0, ""),
        ]);
        let p = peek(&s).expect("строка курсора — файл");
        assert_eq!(p.title, r"crates\urfin-core\src\ops\copy.rs");
        assert_eq!(
            p.loc.path,
            std::path::Path::new(r"C:\Dev\urfin\crates\urfin-core\src\ops\copy.rs")
        );
        let c = peek_cmd(&p, Side::Right);
        assert_eq!(c.id.as_str(), "viewer.quick");
        assert_eq!(c.args.str("side"), Some("right"));
        assert_eq!(c.args.int("line"), Some(142));
        assert_eq!(c.args.str("hit"), Some("spawn"));
    }

    #[test]
    fn preview_without_text_opens_file_start() {
        let mut s = state_with(vec![
            hit("a.rs", urfin_core::EntryKind::File, 0, ""),
            hit("src", urfin_core::EntryKind::Dir, 0, ""),
        ]);
        let p = peek(&s).expect("файл");
        let c = peek_cmd(&p, Side::Left);
        assert_eq!(c.args.int("line"), None, "без строки — начало файла");
        assert_eq!(c.args.str("hit"), None, "без подсветки");
        if let Some(m) = s.ext.get_mut::<SearchModel>() {
            m.set_cursor(1);
        }
        assert_eq!(peek(&s), None, "у папки предпросмотра нет");
    }

    #[test]
    fn run_keeps_flags_of_the_form() {
        let mut sc = Screen::new(Form::default(), Zone::List);
        sc.toggle(Flag::Case);
        sc.toggle(Flag::Regex);
        sc.toggle(Flag::Recursive);
        assert!(sc.form.case && sc.form.regex && sc.form.recursive);
        sc.toggle(Flag::Regex);
        assert!(!sc.form.regex);
        assert_eq!(sc.want, Some(Want::Enter), "в списке фокус не трогаем");
    }

    #[test]
    fn row_cache_keeps_rows_until_the_key_changes() {
        let key = |width: f32| RowKey {
            store: None,
            width: px(width),
            font: gpui_kit::font(MONO),
            pal: Pal {
                text: Hsla::default(),
                secondary: Hsla::default(),
                cursor: Hsla::default(),
                cursor_text: Hsla::default(),
                highlight: Hsla::default(),
            },
        };
        let mut c = RowCache::default();
        let prev = c.frame(key(100.0));
        assert!(prev.is_empty());
        c.rows.insert(7, [None, None]);
        c.done(prev);
        let prev = c.frame(key(100.0));
        assert!(
            prev.contains_key(&7),
            "та же ширина — строки прошлого кадра"
        );
        assert!(c.rows.is_empty() && c.spare.is_empty());
        c.rows.insert(7, [None, None]);
        c.done(prev);
        assert!(c.frame(key(120.0)).is_empty(), "другая ширина — кэш заново");
    }
}
