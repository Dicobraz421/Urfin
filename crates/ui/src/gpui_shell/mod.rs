//! Интерфейс на GPUI (ADR-0018). G-02 — каркас: [`CoreHost`] владеет `Core` и качает
//! его `pump` только по сигналу [`Signal`] (воркеры Core, вторые запуски) — без таймеров
//! опроса; в простое кадров нет. Клавиатура — конвейер keymap S3 ([`input`]).

mod chrome;
mod compare;
mod drives;
mod features;
mod field;
pub(crate) mod input;
mod jobs;
mod menu;
mod overlays;
mod panel;
mod pouch;
mod screens;
mod settings;
mod strip;
mod term_menu;
mod term_view;
mod theme;
mod view;
mod views;
mod welcome;

pub use theme::Theme;

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, AsyncApp, Bounds, ClickEvent, Context, Entity, FocusHandle, IntoElement, KeyUpEvent,
    ModifiersChangedEvent, Render, SharedString, TitlebarOptions, WeakEntity, Window, WindowBounds,
    WindowOptions, div, point, px, size,
};
use urfin_core::{Command, Core, Side, WindowGeom};

use crate::tokens::{Metric, Role};

/// Бюджет `pump` за один проход (как у egui-окна, S1 §1).
const PUMP_BUDGET: Duration = Duration::from_millis(4);
/// Сколько держать прежний кадр панели, пока грузится каталог (мануал 4.8: мигание).
const HOLD_LOADING: Duration = Duration::from_millis(250);
enum Wake {
    Now,
    After(Duration),
    /// Записки клавиш subclass окна (S3-07): разобрать без нажатия gpui.
    Keys,
}

/// `urfin_core::Waker` окна GPUI: сигнал в канал, его ждёт задача основного потока.
#[derive(Debug)]
pub struct Signal {
    tx: UnboundedSender<Wake>,
}

/// Приёмник сигналов [`Signal`]: отдаётся в [`run`].
#[derive(Debug)]
pub struct WakeRx(UnboundedReceiver<Wake>);

impl std::fmt::Debug for Wake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Wake::Now => f.write_str("Now"),
            Wake::After(d) => write!(f, "After({d:?})"),
            Wake::Keys => f.write_str("Keys"),
        }
    }
}

impl Signal {
    /// Пара «будильник для Core — приёмник для окна».
    pub fn new() -> (Arc<Signal>, WakeRx) {
        let (tx, rx) = unbounded();
        (Arc::new(Signal { tx }), WakeRx(rx))
    }

    /// Разобрать записки клавиш, не встретившие нажатия gpui (`Num5` без `NumLock`).
    pub fn keys(&self) {
        let _ = self.tx.unbounded_send(Wake::Keys);
    }
}

impl urfin_core::Waker for Signal {
    fn wake(&self) {
        // Окно закрыто — приёмника нет, будить некого.
        let _ = self.tx.unbounded_send(Wake::Now);
    }

    fn wake_after(&self, d: Duration) {
        let _ = self.tx.unbounded_send(Wake::After(d));
    }
}

/// Что приложение делает на каждом проходе `pump`: вторые запуски (S1-15). `true` —
/// окно вывести вперёд.
pub type Poll = Box<dyn FnMut(&mut Core) -> bool>;

/// Всё, что окну GPUI нужно от приложения.
pub struct Launch {
    pub core: Core,
    pub wake: WakeRx,
    /// Будильник того же `Core`: повторный проход, если `pump` не успел всё.
    pub signal: Arc<Signal>,
    pub poll: Poll,
    /// Конвейер клавиш keymap (S3).
    pub keys: urfin_keymap::KeyInput,
    /// Тема Fluent (токены, раскраска, формат ячеек).
    pub theme: Theme,
    /// После первого кадра; `true` — выйти (замер старта).
    pub first_frame: Option<Box<dyn FnOnce() -> bool>>,
    /// `--bench`: сценарий замера (G-07).
    pub bench: Option<crate::bench::Bench>,
    /// `--screens <каталог>`: снимки сцен для проверки дизайна.
    pub screens: Option<std::path::PathBuf>,
    /// Повышенные права или удалённый сеанс (T-01-5): полоса сверху и метка.
    pub context: Option<crate::sys::Context>,
}

impl std::fmt::Debug for Launch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Launch").finish_non_exhaustive()
    }
}

/// Владелец `Core` в окне GPUI (ADR-0018): `pump` по сигналу, `cx.notify()` — только
/// когда состояние изменилось.
#[allow(
    clippy::struct_excessive_bools,
    reason = "флаги окна и строк — независимые"
)]
pub struct CoreHost {
    core: Option<Core>,
    signal: Arc<Signal>,
    poll: Poll,
    rev: u64,
    /// Сетка панелей (строк в колонке, колонок), отправленная в Core (`panel.viewport`).
    grid: [(u32, u32); 2],
    theme: Theme,
    keybar: crate::keybar::Keybar,
    registered: Vec<&'static str>,
    /// Зажатые модификаторы — подписи F-клавиш.
    mods: crate::keybar::Mods,
    toasts: Vec<overlays::Toast>,
    first_frame: Option<Box<dyn FnOnce() -> bool>>,
    bench: Option<crate::bench::Bench>,
    /// Таймер простоя бенча уже стоит: не ставить второй (иначе таймеры будят окно
    /// цепочкой и простой меряет сам себя).
    bench_timer: bool,
    screens: Option<screens::Screens>,
    /// Открытое поле ввода (черновик, переименование).
    field: Option<field::Field>,
    /// Черновик удаления — сразу насовсем (Shift+F8 или корзины нет).
    ops_permanent: bool,
    /// Превью группового переименования (S5-23).
    mr_preview: Option<field::MrPreview>,
    menu: menu::Menu,
    /// Остаток прокрутки колесом в строках (по панелям).
    wheel_acc: [f32; 2],
    /// Выбор в видимом вопросе.
    choice: overlays::PromptSel,
    focus: FocusHandle,
    input: input::Input,
    out: Vec<Command>,
    /// Панели — кэшируемые view (G-07) и отпечатки их последней отрисовки.
    panels: [[Entity<panel::PanelView>; 3]; 2],
    /// Ширина карточек панелей в этом кадре (путь и колонки по ширине).
    panel_w: [f32; 2],
    prints: [[u64; 3]; 2],
    /// Протяжка с зажатой ПКМ (Far): панель, ставить или снимать пометку, последняя строка.
    rdrag: Option<(Side, bool, String)>,
    /// Фичи окна в своих модулях (ADR-0022): терминал, задачи, ….
    features: features::Features,
    /// Где вызвано контекстное меню мышью (окно, px); Shift+F10 — у строки курсора.
    context_at: Option<(f32, f32)>,
    /// Эксперимент `slide`: где был курсор (таб, верх страницы, строка) и скольжение полосы.
    slot: [Option<(u32, u32, u32)>; 2],
    slide: [Option<Slide>; 2],
    /// Низ окна (cmdline, статус, F-клавиши) — кэшируемые view, отпечатки как у панелей.
    bottom: [Entity<chrome::ChromeView>; 3],
    bottom_print: [u64; 3],
    /// Акцент и контрастность ОС на лету (`sys::watch`, T-01-2).
    looks: std::sync::mpsc::Receiver<crate::sys::Look>,
    context: Option<crate::sys::Context>,
    /// Выбор в списке дисков (стрелки); `None` — предложенный при открытии.
    drive_sel: Option<usize>,
    /// Опции строки копирования (F5/F6, D-04).
    strip_opts: field::StripOpts,
    /// Маска отметки группы, пока строка открыта: совпавшие имена подсвечены (D-04).
    mask_preview: Option<(String, bool)>,
    /// Выбранный вариант дополнения: (поколение списка, номер).
    complete_sel: (u64, usize),
    /// Заметки после имён в панелях (превью переименования, метки сравнения).
    notes: [std::rc::Rc<std::collections::HashMap<String, panel::Note>>; 2],
    /// Строк превью во встроенной строке (дерево F7) — для высоты строки.
    strip_rows: usize,
}

impl std::fmt::Debug for CoreHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreHost")
            .field("rev", &self.rev)
            .finish_non_exhaustive()
    }
}

impl CoreHost {
    fn new(launch: Launch, cx: &mut Context<Self>) -> CoreHost {
        let Launch {
            core,
            wake,
            signal,
            poll,
            keys,
            theme,
            first_frame,
            bench,
            screens,
            context,
        } = launch;
        listen(wake, cx);
        let (look_tx, looks) = std::sync::mpsc::channel();
        let waker = signal.clone();
        crate::sys::watch(move || urfin_core::Waker::wake(&*waker), look_tx);
        let registered = crate::keybar::registered(core.registry());
        let menu = menu::Menu::new(core.registry(), None);
        let host = cx.entity().downgrade();
        let panels = [Side::Left, Side::Right].map(|side| {
            [panel::Piece::Head, panel::Piece::List, panel::Piece::Foot].map(|piece| {
                cx.new(|_| panel::PanelView {
                    host: host.clone(),
                    side,
                    piece,
                })
            })
        });
        let bottom = chrome::Bottom::ALL.map(|part| {
            cx.new(|_| chrome::ChromeView {
                host: host.clone(),
                part,
            })
        });
        CoreHost {
            core: Some(core),
            signal,
            poll,
            rev: u64::MAX,
            grid: [(0, 0); 2],
            theme,
            keybar: crate::keybar::Keybar::default(),
            registered,
            mods: crate::keybar::Mods::None,
            toasts: Vec::new(),
            first_frame,
            bench,
            bench_timer: false,
            screens: screens.map(screens::Screens::new),
            field: None,
            ops_permanent: false,
            mr_preview: None,
            menu,
            wheel_acc: [0.0; 2],
            choice: overlays::PromptSel::NONE,
            focus: cx.focus_handle(),
            input: input::Input::new(keys),
            out: Vec::new(),
            panels,
            panel_w: [0.0; 2],
            prints: [[0; 3]; 2],
            rdrag: None,
            features: features::Features::new(),
            context_at: None,
            slot: [None; 2],
            slide: [None, None],
            bottom,
            bottom_print: [0; 3],
            looks,
            context,
            drive_sel: None,
            strip_opts: field::StripOpts::default(),
            mask_preview: None,
            complete_sel: (0, 0),
            notes: Default::default(),
            strip_rows: 0,
        }
    }

    /// Какие панели видны и их ширины: Ctrl+O — ни одной; узкое окно (< 720) — только
    /// активная; иначе — по `ui.split` (S2-07). Панели — во всю ширину окна, между
    /// ними линия 1 px (D-02).
    /// Список дополнений поля (№70): плашка с тенью от поля — вниз у полей сверху
    /// (путь панели), вверх у полей внизу окна (встроенные строки); общая часть —
    /// `highlight`, справа — источник.
    #[allow(clippy::too_many_lines, reason = "строка варианта слева направо")]
    #[allow(clippy::many_single_char_names, reason = "геометрия списка")]
    fn completion_list(&self, window: &Window, cx: &mut Context<Self>) -> Option<gpui_kit::Div> {
        let c = self.completion()?;
        let b = self.field.as_ref()?.bounds.get()?;
        if c.items.is_empty() {
            return None;
        }
        let i18n = urfin_i18n::current();
        let theme = &self.theme;
        let win = window.viewport_size();
        let (fx, fy, fw, fh) = (
            f32::from(b.origin.x),
            f32::from(b.origin.y),
            f32::from(b.size.width),
            f32::from(b.size.height),
        );
        let w = fw.max(COMPLETE_W).min(f32::from(win.width));
        let x = fx.min(f32::from(win.width) - w).max(0.0);
        #[allow(clippy::cast_precision_loss, reason = "вариантов десяток")]
        let h = c.items.len() as f32 * pouch::H_ITEM + 8.0;
        let up = fy > f32::from(win.height) / 2.0;
        let y = if up { fy - h - 2.0 } else { fy + fh + 2.0 };
        let sel = if self.complete_sel.0 == c.generation {
            self.complete_sel.1
        } else {
            0
        };
        let typed = c.text.clone();
        let kind = c.kind;
        let mut body = div().flex().flex_col();
        for (i, cand) in c.items.iter().enumerate() {
            let glyph = if cand.history {
                "\u{E81C}"
            } else if kind == crate::field_history::FieldKind::Path {
                "\u{E8B7}"
            } else {
                "\u{E8A5}"
            };
            let source = i18n.get(if cand.history {
                "ui-complete-history"
            } else if kind == crate::field_history::FieldKind::Path {
                "ui-complete-folder"
            } else {
                "ui-complete-here"
            });
            // Общая часть с набранным — `highlight`.
            let common = if !c.history_only
                && cand.text.to_lowercase().starts_with(&typed.to_lowercase())
                && cand.text.is_char_boundary(typed.len())
            {
                typed.len()
            } else {
                0
            };
            let pick = cand.text.clone();
            body = body.child(
                div()
                    .id(SharedString::from(format!("complete-{i}")))
                    .h(px(pouch::H_ITEM))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .rounded(px(4.0))
                    .when(i == sel, |d| d.bg(theme.c(Role::FillCursor)))
                    .when(i != sel, |d| {
                        d.hover(|d| d.bg(theme.c(Role::FillSubtleHover)))
                    })
                    .child(
                        div()
                            .flex_none()
                            .font_family("Segoe Fluent Icons")
                            .text_size(px(12.0))
                            .text_color(theme.c(Role::TextSecondary))
                            .child(SharedString::new_static(glyph)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .font_family("Cascadia Mono")
                            .text_size(px(12.5))
                            .when(common > 0, |d| {
                                d.child(
                                    div()
                                        .bg(theme.c(Role::Highlight))
                                        .child(SharedString::from(cand.text[..common].to_owned())),
                                )
                            })
                            .child(SharedString::from(cand.text[common..].to_owned())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(12.0))
                            .text_color(theme.c(Role::TextSecondary))
                            .child(SharedString::from(source)),
                    )
                    .on_click(cx.listener(move |h, _: &ClickEvent, window, cx| {
                        let cmds = h.complete_insert(&pick, kind, window, cx);
                        for c in cmds {
                            h.run(c, window, cx);
                        }
                    })),
            );
        }
        Some(pouch::pouch(theme, x, y, w, None, body))
    }

    /// Список «Если файл есть» строки F5/F6: плашка над кнопкой, ✓ у выбранного.
    fn conflict_list(&self, cx: &mut Context<Self>) -> Option<gpui_kit::Div> {
        if !self.strip_opts.dropdown {
            return None;
        }
        let b = self.field.as_ref()?.drop.get()?;
        let i18n = urfin_i18n::current();
        let theme = &self.theme;
        #[allow(clippy::cast_precision_loss, reason = "четыре пункта")]
        let h = field::CONFLICT.len() as f32 * pouch::H_ITEM + 8.0;
        let (x, y) = (f32::from(b.origin.x), f32::from(b.origin.y) - h - 2.0);
        let mut body = div().flex().flex_col();
        for (i, (_, label)) in field::CONFLICT.iter().enumerate() {
            let on = self.strip_opts.conflict == i;
            body = body.child(
                div()
                    .id(SharedString::from(format!("ifexists-{i}")))
                    .h(px(pouch::H_ITEM))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .rounded(px(4.0))
                    .when(on, |d| d.bg(theme.c(Role::FillCursor)))
                    .when(!on, |d| d.hover(|d| d.bg(theme.c(Role::FillSubtleHover))))
                    .child(
                        div()
                            .w(px(14.0))
                            .flex_none()
                            .font_family("Segoe Fluent Icons")
                            .text_size(px(11.0))
                            .child(SharedString::new_static(if on { "\u{E73E}" } else { "" })),
                    )
                    .child(SharedString::from(i18n.get(label)))
                    .on_click(cx.listener(move |h, _: &ClickEvent, _, cx| {
                        h.strip_opts.conflict = i;
                        h.strip_opts.dropdown = false;
                        cx.notify();
                    })),
            );
        }
        Some(pouch::pouch(theme, x, y, CONFLICT_W, None, body))
    }

    /// Заметки после имён: превью группового переименования в активной панели, метки
    /// сравнения (фича `compare`).
    fn compute_notes(
        &self,
        s: &urfin_core::AppState,
    ) -> [std::rc::Rc<std::collections::HashMap<String, panel::Note>>; 2] {
        use std::collections::HashMap;
        let mut out: [HashMap<String, panel::Note>; 2] = Default::default();
        let draft = s.ext.get::<crate::UiState>().and_then(|u| u.draft.as_ref());
        if let Some(p) = self.mr_preview.as_ref()
            && draft.is_some_and(|d| d.cmd == field::MULTI_RENAME)
        {
            let i = usize::from(s.active == Side::Right);
            for (from, to, problem) in &p.rows {
                out[i].insert(
                    from.clone(),
                    panel::Note::Rename {
                        to: to.clone(),
                        bad: !problem.is_empty(),
                    },
                );
            }
        }
        if let Some(c) = self.features.get::<compare::Compare>() {
            c.notes(s, &mut out);
        }
        out.map(std::rc::Rc::new)
    }

    /// Левый край и ширина панелей по их ширинам (линия между ними — `WDivider`).
    fn panel_spans(theme: &Theme, widths: [Option<f32>; 2]) -> [Option<(f32, f32)>; 2] {
        let left = widths[0].map(|w| (0.0, w));
        let right = widths[1].map(|w| {
            let x = widths[0].map_or(0.0, |l| l + theme.m(Metric::WDivider));
            (x, w)
        });
        [left, right]
    }

    fn layout(theme: &Theme, s: &urfin_core::AppState, width: f32) -> [Option<f32>; 2] {
        let st = s.ext.get::<crate::UiState>();
        if st.is_some_and(|u| u.panels_hidden) {
            return [None, None];
        }
        let gap = theme.m(Metric::WDivider);
        let avail = width;
        let shown = [s.panel(Side::Left).visible, s.panel(Side::Right).visible];
        let narrow = width < crate::layout::SINGLE_PANEL_BELOW;
        match (shown, narrow) {
            ([true, true], false) => {
                let split = st.map_or(0.5, |u| u.split).clamp(0.1, 0.9);
                let l = (avail - gap) * split;
                [Some(l), Some(avail - gap - l)]
            }
            ([true, true], true) => {
                if s.active == Side::Left {
                    [Some(avail), None]
                } else {
                    [None, Some(avail)]
                }
            }
            ([true, false], _) => [Some(avail), None],
            ([false, true], _) => [None, Some(avail)],
            _ => [None, None],
        }
    }

    /// Сетка каждой панели — в Core (`panel.viewport`): по ней он держит курсор в
    /// видимой странице и считает PgUp/PgDn и ←/→ в колонках.
    fn sync_viewport(&mut self, window: &Window) {
        let Some(core) = self.core.as_mut() else {
            return;
        };
        let size = window.viewport_size();
        let widths = Self::layout(&self.theme, core.state(), f32::from(size.width));
        let h = f32::from(size.height)
            - self.theme.m(Metric::HTitlebar)
            - self.theme.m(Metric::HCmdline)
            - self.theme.m(Metric::HStatus)
            - self.theme.m(Metric::HKeybar);
        let cover = strip::cover(
            core.state(),
            self.strip_rows,
            self.features
                .get::<compare::Compare>()
                .is_some_and(|c| c.mode(core.state())),
        );
        for (i, side) in [Side::Left, Side::Right].into_iter().enumerate() {
            let mode = core.state().panel(side).tab().view_mode;
            let Some(w) = widths[i] else {
                continue;
            };
            self.panel_w[i] = w;
            // Встроенная строка накрывает низ панели: курсор держится над ней (D-04).
            let under = cover
                .filter(|(scope, _)| scope.covers(side))
                .map_or(0.0, |(_, h)| h);
            let g = panel::grid(&self.theme, mode, w, h - under);
            if g == self.grid[i] {
                continue;
            }
            self.grid[i] = g;
            core.dispatch(
                Command::new("panel.viewport")
                    .arg("side", if i == 0 { "left" } else { "right" })
                    .arg("rows", i64::from(g.0))
                    .arg("cols", i64::from(g.1)),
            );
            urfin_core::Waker::wake(&*self.signal);
        }
    }

    /// Проход Core: вторые запуски, `pump`, выход по `quit`.
    fn pump(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_mut() else {
            return;
        };
        if (self.poll)(core) {
            window.activate_window();
        }
        let p = core.pump(PUMP_BUDGET);
        if p.more {
            urfin_core::Waker::wake(&*self.signal);
        }
        if core.state().quit {
            self.shutdown(cx);
            return;
        }
        self.take_events(cx);
        self.run_due();
        // Режим вида мог смениться: сетка панели другая.
        self.sync_viewport(window);
        self.sync_features(window);
        self.sync_inbox(window);
        let Some(core) = self.core.as_ref() else {
            return;
        };
        let rev = core.state().rev;
        if rev != self.rev {
            self.rev = rev;
            cx.notify();
        }
    }

    /// Размер окна — фичам (размер PTY и т. п.).
    fn sync_features(&mut self, window: &mut Window) {
        let Some(core) = self.core.as_mut() else {
            return;
        };
        for f in self.features.iter_mut() {
            if let Some(c) = f.sync(&self.theme, window) {
                core.dispatch(c);
            }
        }
    }

    /// Команды фич по времени (пауза курсора быстрого просмотра): в Core, разбудить.
    fn run_due(&mut self) {
        let Some(core) = self.core.as_mut() else {
            return;
        };
        let now = std::time::Instant::now();
        let mut any = false;
        for f in self.features.iter_mut() {
            for c in f.due(core.state(), now) {
                core.dispatch(c);
                any = true;
            }
        }
        if any {
            urfin_core::Waker::wake(&*self.signal);
        }
    }

    /// Поля ввода фич (строка «Найти» вьювера) — под состояние Core.
    fn sync_feature_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else {
            return;
        };
        for f in self.features.iter_mut() {
            f.input(core.state(), &self.focus, window, cx);
        }
    }

    /// События фич в очереди Core (размер окна вьювера): отправлено — разбудить Core.
    fn sync_inbox(&mut self, window: &mut Window) {
        let Some(core) = self.core.as_ref() else {
            return;
        };
        let mut sent = false;
        for f in self.features.iter_mut() {
            sent |= f.inbox(core.state(), &self.theme, window);
        }
        if sent {
            urfin_core::Waker::wake(&*self.signal);
        }
    }

    fn dispatch(&mut self, cmd: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(core) = self.core.as_mut() {
            core.dispatch(Command::new(cmd));
        }
        self.pump(window, cx);
    }

    /// События UI из Core: тосты (дедлайн — будильником); job-бар ≤ 30 кадров/с.
    #[allow(clippy::too_many_lines, reason = "разбор событий Core — одна таблица")]
    fn take_events(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_mut() else {
            return;
        };
        let now = std::time::Instant::now();
        let mut drafts = Vec::new();
        let filter_now = core
            .state()
            .active_tab()
            .filter
            .as_ref()
            .map(|m| m.as_str().to_owned());
        for ev in core.take_ui_events() {
            // Сначала фичи окна (ADR-0022): событие своей фичи — её командами.
            if let urfin_core::UiEvent::Ext { id, args } = &ev
                && let Some(cmds) = self.features.ext(id.as_str(), args, core.state(), cx)
            {
                cx.notify();
                drafts.extend(cmds);
                continue;
            }
            match ev {
                urfin_core::UiEvent::Toast { level, text } => {
                    self.toasts.push(overlays::Toast::new(level, text, now));
                    cx.notify();
                }
                // Черновик операции (F5/F6/F7/F8): строка с полем или кнопками (S2-15).
                urfin_core::UiEvent::Ext { id, args }
                    if id.as_str() == "ops.multi_rename.preview" =>
                {
                    self.mr_preview = Some(field::MrPreview::from_args(&args));
                    cx.notify();
                }
                // Ctrl+I: строка фильтра с текущей маской таба.
                urfin_core::UiEvent::Ext { id, .. } if id.as_str() == "panel.ask_filter" => {
                    let mut c = Command::new("ui.draft")
                        .arg("command", "panel.filter")
                        .arg("arg", "mask");
                    if let Some(m) = &filter_now {
                        c = c.arg("init", m.as_str());
                    }
                    drafts.push(c);
                }
                urfin_core::UiEvent::Ext { id, args } if id.as_str() == "ops.draft" => {
                    let command = args.str("command").unwrap_or_default().to_owned();
                    let arg = match command.as_str() {
                        "ops.copy" | "ops.move" => "dest",
                        "ops.mkdir" => "name",
                        field::MULTI_RENAME => "pattern",
                        "ops.link" => "dest",
                        "ops.attrs" => "set",
                        "ops.touch" => "mtime",
                        _ => "confirmed",
                    };
                    self.ops_permanent = args.bool("permanent").unwrap_or(false);
                    let mut c = Command::new("ui.draft")
                        .arg("command", command.as_str())
                        .arg("arg", arg);
                    if let Some(init) = args.str("init") {
                        c = c.arg("init", init);
                    }
                    drafts.push(c);
                }
                _ => {}
            }
        }
        if !drafts.is_empty() {
            for c in drafts {
                core.dispatch(c);
            }
            urfin_core::Waker::wake(&*self.signal);
            cx.notify();
        }
        let before = self.toasts.len();
        self.toasts.retain(|t| t.until > now);
        if self.toasts.len() != before {
            cx.notify();
        }
        if let Some(next) = self.toasts.iter().map(|t| t.until).min() {
            urfin_core::Waker::wake_after(&*self.signal, next - now);
        }
        for f in self.features.iter_mut() {
            if let Some(d) = f.tick(core.state(), now) {
                urfin_core::Waker::wake_after(&*self.signal, d);
                cx.notify();
            }
        }
    }

    /// Клавиши видимого вопроса: Enter — выбранный, Esc — отмена, ←/→/Tab — выбор,
    /// буква варианта — ответ; Пробел — флажок «для всех», Shift+буква — ответ
    /// для всех.
    fn prompt_key(&mut self, ks: &gpui_kit::Keystroke) -> Option<Command> {
        let p = self.core.as_ref()?.state().prompts.active()?;
        let n = p.spec.choices.len();
        self.choice = self.choice.of(p);
        let cur = &mut self.choice;
        match ks.key.as_str() {
            "escape" => return Some(overlays::cancel(p)),
            "enter" => return overlays::reply(p, cur.sel, cur.all),
            "left" => cur.sel = cur.sel.saturating_sub(1),
            "right" | "tab" => cur.sel = (cur.sel + 1).min(n.saturating_sub(1)),
            "space" => cur.all = !cur.all && p.spec.allow_all,
            // «Пропустить все такие» (D-04, `screen-operror.png`).
            "a" if p.spec.allow_all && overlays::error_line(p) => {
                let i = p.spec.choices.iter().position(|c| c.id == "skip")?;
                return overlays::reply(p, i, true);
            }
            k => {
                let typed = ks.key_char.as_deref().unwrap_or(k).to_lowercase();
                let i = p.spec.choices.iter().position(|c| {
                    let key = c.key.to_lowercase().to_string();
                    key == k || key == typed
                })?;
                return overlays::reply(p, i, cur.all || ks.modifiers.shift);
            }
        }
        None
    }

    /// Несколько команд мыши подряд.
    fn run_all(&mut self, cmds: Vec<Command>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(core) = self.core.as_mut() {
            for c in cmds {
                core.dispatch(c.from(urfin_core::Origin::Mouse));
            }
        }
        self.pump(window, cx);
    }

    /// Клик по строке: панель — активной, курсор — на строку; Ctrl — пометка (Shift —
    /// на нажатии, [`Self::row_mark_down`]); двойной — вход (S2 §2.1, S0 №74).
    fn row_click(
        &mut self,
        side: Side,
        name: Option<&str>,
        clicks: usize,
        ctrl: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut cmds = self.focus_and_cursor(side, name);
        if ctrl && name.is_some() {
            cmds.push(Command::new("sel.toggle"));
        }
        if clicks >= 2 {
            cmds.push(Command::new("panel.enter"));
        }
        self.run_all(cmds, window, cx);
    }

    /// Shift+ЛКМ — пометка строки (S0 №74, вместо ПКМ Far); `..` не помечается.
    /// Протяжка с зажатой кнопкой ставит строкам то же состояние, что получила эта.
    fn row_mark_down(
        &mut self,
        side: Side,
        name: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut cmds = self.focus_and_cursor(side, name);
        if name.is_some() {
            cmds.push(Command::new("sel.toggle"));
        }
        self.run_all(cmds, window, cx);
        self.rdrag = name
            .zip(self.marked(side))
            .map(|(n, on)| (side, on, n.to_owned()));
    }

    /// ПКМ — курсор на строку и контекстное меню у мыши (S0 №74).
    fn row_context(
        &mut self,
        side: Side,
        name: Option<&str>,
        at: (f32, f32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut cmds = self.focus_and_cursor(side, name);
        cmds.push(Command::new("ui.menu").arg("menu", "context"));
        self.run_all(cmds, window, cx);
        self.context_at = Some(at);
    }

    /// Строка курсора панели помечена; `None` — на `..` или пусто.
    fn marked(&self, side: Side) -> Option<bool> {
        let t = self.core.as_ref()?.state().panel(side).tab();
        match t.current()? {
            urfin_core::Row::Entry { idx, .. } => Some(t.selection.contains(idx)),
            urfin_core::Row::Parent => None,
        }
    }

    /// Мышь с зажатой ЛКМ после Shift+ЛКМ над строкой: курсор на неё, пометка — как у
    /// первой строки.
    fn row_right_drag(
        &mut self,
        side: Side,
        name: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((s, on, last)) = self.rdrag.as_ref() else {
            return;
        };
        let (on, Some(name)) = (*on, name) else {
            return;
        };
        if *s != side || last == name {
            return;
        }
        if let Some(d) = self.rdrag.as_mut() {
            name.clone_into(&mut d.2);
        }
        self.run_all(self.focus_and_cursor(side, Some(name)), window, cx);
        if self.marked(side) == Some(!on) {
            self.run_all(vec![Command::new("sel.toggle")], window, cx);
        }
    }

    /// Идёт протяжка пометки (Shift+ЛКМ).
    fn marking(&self) -> bool {
        self.rdrag.is_some()
    }

    /// ЛКМ отпущена (или мышь вернулась без неё): протяжка кончилась.
    fn right_drag_end(&mut self) {
        self.rdrag = None;
    }

    fn focus_and_cursor(&self, side: Side, name: Option<&str>) -> Vec<Command> {
        let active = self.core.as_ref().is_some_and(|c| c.state().active == side);
        let mut cmds = Vec::with_capacity(3);
        if !active {
            cmds.push(Command::new("panel.focus").arg("side", chrome::side_name(side)));
        }
        cmds.push(match name {
            Some(n) => Command::new("panel.cursor.to").arg("name", n),
            None => Command::new("panel.cursor.home"),
        });
        cmds
    }

    /// Колесо: строки копятся с дробной частью, сдвиг — `panel.viewport{top}`.
    fn wheel(
        &mut self,
        side: Side,
        ev: &gpui_kit::ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let row_h = self.theme.m(Metric::HRow);
        let dy = f32::from(ev.delta.pixel_delta(px(row_h)).y);
        let i = usize::from(side == Side::Right);
        self.wheel_acc[i] -= dy / row_h;
        let whole = self.wheel_acc[i].trunc();
        self.wheel_acc[i] -= whole;
        #[allow(clippy::cast_possible_truncation, reason = "строк за событие немного")]
        let lines = whole as i64;
        let Some(core) = self.core.as_ref() else {
            return;
        };
        if lines == 0 {
            return;
        }
        let t = core.state().panel(side).tab();
        let top = (i64::from(t.cursor.top) + lines).max(0);
        let (rows, cols) = self.grid[i];
        self.run_all(
            vec![
                Command::new("panel.viewport")
                    .arg("side", chrome::side_name(side))
                    .arg("rows", i64::from(rows))
                    .arg("cols", i64::from(cols))
                    .arg("top", top),
            ],
            window,
            cx,
        );
    }

    /// Тема кадра (T-01): выбор `ui.theme.*`, светлая/тёмная ОС (gpui следит за окном),
    /// акцент и контрастность из `sys::watch`, свои темы, высота строки.
    fn sync_theme(&mut self, window: &Window) {
        while let Ok(look) = self.looks.try_recv() {
            self.theme.set_look(&look);
        }
        let os_dark = matches!(
            window.appearance(),
            gpui_kit::WindowAppearance::Dark | gpui_kit::WindowAppearance::VibrantDark
        );
        let Some(st) = self
            .core
            .as_ref()
            .and_then(|c| c.state().ext.get::<crate::UiState>())
        else {
            return;
        };
        self.theme
            .sync(&st.theme, os_dark, &st.user_themes, st.density);
    }

    /// Шаг сценария `--bench` на кадре: команды, размер окна, простой или отчёт и выход.
    fn drive_bench(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(b) = self.bench.as_mut() else {
            return;
        };
        match b.frame(std::time::Instant::now()) {
            crate::bench::Step::Next => window.request_animation_frame(),
            crate::bench::Step::Cmds(cmds) => {
                if let Some(core) = self.core.as_mut() {
                    for c in cmds {
                        core.dispatch(c);
                    }
                    core.pump(PUMP_BUDGET);
                }
                window.request_animation_frame();
            }
            crate::bench::Step::Resize(w, h) => {
                window.resize(size(px(w), px(h)));
                window.request_animation_frame();
            }
            crate::bench::Step::Idle(d) => {
                // Простой: кадров не просим; один таймер разбудит, кадр — по notify.
                if self.bench_timer {
                    return;
                }
                self.bench_timer = true;
                let this = cx.entity().downgrade();
                cx.spawn_in(window, async move |_, cx| {
                    cx.background_executor().timer(d).await;
                    let _ = this.update(cx, |h, cx| {
                        h.bench_timer = false;
                        cx.notify();
                    });
                })
                .detach();
            }
            crate::bench::Step::Done(report) => {
                println!("bench ui=gpui {report}");
                tracing::info!(%report, "bench");
                self.bench = None;
                self.shutdown(cx);
            }
        }
    }

    /// Шаг `--screens` на кадре: команды сцены, ожидание, снимок в фоне.
    fn drive_screens(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sc) = self.screens.as_mut() else {
            return;
        };
        let ready = self.core.as_ref().is_some_and(|c| sc.ready(c.state()));
        match sc.frame(ready) {
            screens::Step::Enter(cmds, to, context) => {
                if let Some((w, h)) = to {
                    window.resize(size(px(w), px(h)));
                }
                if let Some(screens::SetContext(c)) = context {
                    self.context = c;
                }
                // Тосты прошлой сцены ещё живы (сцены идут быстрее их срока) — снимок без них.
                self.toasts.clear();
                // Через `run_all`, как клик: `pump` окна — сетка панелей, черновики, тосты.
                self.run_all(cmds, window, cx);
                window.request_animation_frame();
            }
            screens::Step::Next => window.request_animation_frame(),
            screens::Step::Shoot(path) => {
                let hwnd = hwnd_of(window);
                let task = cx.background_spawn(async move { screens::shoot(hwnd, &path) });
                cx.spawn_in(window, async move |this, cx| {
                    let r = task.await;
                    let _ = this.update(cx, |h, cx| {
                        if h.screens.as_mut().is_some_and(|s| s.shot(r)) {
                            h.screens = None;
                            h.shutdown(cx);
                        } else {
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            screens::Step::Idle => {}
        }
    }

    /// Команда от мыши (табы, F-клавиши).
    fn run(&mut self, cmd: Command, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(core) = self.core.as_mut() {
            core.dispatch(cmd.from(urfin_core::Origin::Mouse));
        }
        self.pump(window, cx);
    }

    /// Сессия на диск и выход из цикла GPUI.
    fn shutdown(&mut self, cx: &mut Context<Self>) {
        if let Some(core) = self.core.take() {
            let session = core.shutdown(Duration::from_millis(500));
            tracing::info!(window = ?session.window, "session saved on exit");
        }
        cx.quit();
    }

    /// Геометрия окна — в сессию (размер развёрнутого не сохраняется).
    fn track_bounds(&mut self, window: &mut Window) {
        let Some(core) = self.core.as_mut() else {
            return;
        };
        let last = core.window();
        let g = match window.window_bounds() {
            WindowBounds::Windowed(b) => WindowGeom {
                x: f32::from(b.origin.x),
                y: f32::from(b.origin.y),
                w: f32::from(b.size.width),
                h: f32::from(b.size.height),
                maximized: false,
                split: last.map_or(0.5, |g| g.split),
            },
            WindowBounds::Maximized(_) | WindowBounds::Fullscreen(_) => match last {
                Some(g) => WindowGeom {
                    maximized: true,
                    ..g
                },
                None => return,
            },
        };
        core.set_window(g);
    }

    /// Команды keymap — в Core, проход `pump`; дедлайн аккорда — будильник.
    fn after_input(
        &mut self,
        o: Option<input::Outcome>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(core) = self.core.as_mut() {
            for c in self.out.drain(..) {
                core.dispatch(c);
            }
        }
        if let Some(t) = o.and_then(|o| o.wake_at) {
            let d = t.saturating_duration_since(std::time::Instant::now());
            urfin_core::Waker::wake_after(&*self.signal, d);
        }
        self.pump(window, cx);
    }

    /// Нажатие (перехват до привязок gpui): `true` — keymap забрал его.
    fn key_down(
        &mut self,
        ks: &gpui_kit::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(core) = self.core.as_ref() else {
            return false;
        };
        // Поле ввода или черновик: Enter — выполнить, Esc — закрыть; остальное — полю.
        let editing = self.field.is_some()
            || core
                .state()
                .ext
                .get::<crate::UiState>()
                .is_some_and(|u| u.draft.is_some());
        if editing {
            // Ниже клавиши разбирает не keymap: записки subclass этого нажатия не нужны.
            self.input.skip();
            // Дополнение поля (№70) — раньше Enter/Esc поля.
            if let Some(cmds) = self.completion_key(ks, window, cx) {
                self.out
                    .extend(cmds.into_iter().map(|c| c.from(urfin_core::Origin::Key)));
                self.after_input(None, window, cx);
                cx.notify();
                return true;
            }
            let cmds = match ks.key.as_str() {
                "escape" => vec![Command::new("ui.cancel")],
                "enter" => {
                    let mut c: Vec<Command> = self.field_remember(cx).into_iter().collect();
                    c.extend(self.field_commit(ks.modifiers.shift, cx));
                    c
                }
                "tab" if self.field_tab(cx) => return true,
                _ => return false,
            };
            self.out
                .extend(cmds.into_iter().map(|c| c.from(urfin_core::Origin::Key)));
            self.after_input(None, window, cx);
            return true;
        }
        // Список дисков (Alt+F1/F2, D-03) разбирает клавиши сам.
        if let Some(d) = core
            .state()
            .ext
            .get::<crate::UiState>()
            .and_then(|u| u.drives.as_ref())
        {
            self.input.skip();
            let sel = drives::selected(self.drive_sel, d);
            let (cmds, next) = drives::key(d, sel, ks);
            self.drive_sel = Some(next);
            self.out
                .extend(cmds.into_iter().map(|c| c.from(urfin_core::Origin::Key)));
            self.after_input(None, window, cx);
            cx.notify();
            return true;
        }
        // Фичи окна (ADR-0022), по порядку списка: раскрытый список задач (F11, D-06),
        // настройки (D-08), приветствие (D-02), строка сравнения (D-04).
        if let Some(cmds) = self.features.key(core.state(), ks) {
            self.input.skip();
            self.out
                .extend(cmds.into_iter().map(|c| c.from(urfin_core::Origin::Key)));
            self.after_input(None, window, cx);
            cx.notify();
            return true;
        }
        // Поле фичи (строка «Найти» вьювера): остальные клавиши — ему, не keymap.
        if self.features.editing(core.state()) {
            self.input.skip();
            return false;
        }
        // Открытое меню F9 разбирает клавиши само (S2 §2.6).
        if let Some(open) = core
            .state()
            .ext
            .get::<crate::UiState>()
            .and_then(crate::menu::open_id)
            .map(str::to_owned)
        {
            self.input.skip();
            let cmds = self.menu.key(&open, core.state(), ks);
            self.out
                .extend(cmds.into_iter().map(|c| c.from(urfin_core::Origin::Key)));
            self.after_input(None, window, cx);
            cx.notify();
            return true;
        }
        // Видимый вопрос разбирает клавиши сам (S2-15).
        if core.state().prompts.active().is_some() {
            self.input.skip();
            if let Some(cmd) = self.prompt_key(ks) {
                self.out.push(cmd.from(urfin_core::Origin::Key));
            }
            self.after_input(None, window, cx);
            cx.notify();
            return true;
        }
        let o = self.input.key_down(ks, core.state(), &mut self.out);
        let consumed = o.consumed;
        self.after_input(Some(o), window, cx);
        consumed
    }

    fn key_up(&mut self, ev: &KeyUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(core) = self.core.as_ref() {
            self.input
                .key_up(&ev.keystroke, core.state(), &mut self.out);
        }
        self.after_input(None, window, cx);
    }

    fn modifiers(
        &mut self,
        ev: &ModifiersChangedEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let m = ev.modifiers;
        let mods = match (m.shift, m.control, m.alt) {
            (true, false, false) => crate::keybar::Mods::Shift,
            (false, true, false) => crate::keybar::Mods::Ctrl,
            (false, false, true) => crate::keybar::Mods::Alt,
            _ => crate::keybar::Mods::None,
        };
        if mods != self.mods {
            self.mods = mods;
            cx.notify();
        }
        let o = match self.core.as_ref() {
            Some(core) => self
                .input
                .modifiers(ev.modifiers, core.state(), &mut self.out),
            None => return,
        };
        self.after_input(Some(o), window, cx);
    }

    /// Записки subclass без нажатия gpui (`Num5` без `NumLock`, отпускание одного из
    /// двух Ctrl): синтетические сочетания — в Core. Пусто — без прохода `pump`.
    fn flush_keys(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else {
            return;
        };
        let o = self.input.flush(core.state(), &mut self.out);
        if !self.out.is_empty() || o.wake_at.is_some() {
            self.after_input(Some(o), window, cx);
        }
    }

    fn activation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.is_window_active() {
            // Свои темы правили снаружи — перечитать, если mtime каталога изменился (T-01-4).
            self.run(
                Command::new("ui.theme.reload").arg("if_changed", true),
                window,
                cx,
            );
            return;
        }
        if let Some(core) = self.core.as_ref() {
            self.input.focus_lost(core.state(), &mut self.out);
        }
        self.after_input(None, window, cx);
    }
}

/// Сигналы Core → проход `pump` на основном потоке. Очередь сигналов сливается в один
/// проход; `wake_after` — отдельная задача с таймером исполнителя.
fn listen(wake: WakeRx, cx: &mut Context<CoreHost>) {
    let mut rx = wake.0;
    cx.spawn(async move |this: WeakEntity<CoreHost>, cx: &mut AsyncApp| {
        while let Some(first) = rx.next().await {
            let mut now = false;
            let mut keys = false;
            let mut later = Vec::new();
            let mut item = Some(first);
            while let Some(w) = item {
                match w {
                    Wake::Now => now = true,
                    Wake::After(d) => later.push(d),
                    Wake::Keys => keys = true,
                }
                item = rx.try_recv().ok();
            }
            for d in later {
                let this = this.clone();
                cx.spawn(async move |cx: &mut AsyncApp| {
                    cx.background_executor().timer(d).await;
                    pump_in_window(&this, cx);
                })
                .detach();
            }
            if keys && !in_window(&this, cx, CoreHost::flush_keys) {
                break;
            }
            if now && !pump_in_window(&this, cx) {
                break;
            }
        }
    })
    .detach();
}

/// Проход `pump` в окне хоста; `false` — окна или хоста больше нет.
fn pump_in_window(this: &WeakEntity<CoreHost>, cx: &mut AsyncApp) -> bool {
    in_window(this, cx, CoreHost::pump)
}

/// `f` над хостом в его окне; `false` — окна или хоста больше нет.
fn in_window(
    this: &WeakEntity<CoreHost>,
    cx: &mut AsyncApp,
    f: impl FnOnce(&mut CoreHost, &mut Window, &mut Context<CoreHost>),
) -> bool {
    let Some(entity) = this.upgrade() else {
        return false;
    };
    let Some(handle) = cx.update(|cx| cx.windows().first().copied()) else {
        return false;
    };
    handle
        .update(cx, |_, window, cx| {
            entity.update(cx, |host, cx| f(host, window, cx));
        })
        .is_ok()
}

/// View в кадре: отпечаток тот же — из кэша gpui (без раскладки и paint), изменился —
/// рисуется заново. `notify` дочерней view из `render` родителя сработал бы только в
/// следующем кадре, поэтому изменённая view в этом кадре идёт без `cached`.
fn cached<V: Render>(view: &Entity<V>, print: u64, last: &mut u64) -> gpui_kit::AnyElement {
    if std::mem::replace(last, print) == print {
        view.clone()
            .cached(gpui_kit::StyleRefinement::default().size_full())
            .into_any_element()
    } else {
        view.clone().into_any_element()
    }
}

/// Скольжение полосы курсора (эксперимент `slide`): строки страницы «откуда» и «куда».
#[derive(Clone, Copy)]
struct Slide {
    from: f32,
    to: f32,
    at: std::time::Instant,
}

/// Длительность скольжения: короче шага автоповтора (~31 мс).
const SLIDE: Duration = Duration::from_millis(28);

impl CoreHost {
    /// Положение полосы курсора на странице (дробная строка) во время скольжения.
    fn slide_pos(&self, side: Side) -> Option<f32> {
        let sl = self.slide[usize::from(side == Side::Right)]?;
        let p = (sl.at.elapsed().as_secs_f32() / SLIDE.as_secs_f32()).min(1.0);
        // ease-out: быстро стартует, мягко садится.
        let e = 1.0 - (1.0 - p) * (1.0 - p);
        Some(sl.from + (sl.to - sl.from) * e)
    }

    /// Курсор сдвинулся в пределах страницы и колонки — начать скольжение; кадры — до конца.
    fn track_slide(&mut self, window: &mut Window) {
        if !panel::cursor_mode().1 {
            return;
        }
        let Some(core) = self.core.as_ref() else {
            return;
        };
        let s = core.state();
        for (i, side) in [Side::Left, Side::Right].into_iter().enumerate() {
            let t = s.panel(side).tab();
            let rows = self.grid[i].0.max(1);
            let now = (t.id.0, t.cursor.top, t.cursor.row);
            if let Some((tab, top, row)) = self.slot[i]
                && (tab, top) == (now.0, now.1)
                && row != now.2
                && row >= top
                && now.2 >= top
                && (row - top) / rows == (now.2 - top) / rows
            {
                #[allow(clippy::cast_precision_loss, reason = "строки страницы")]
                let from = self.slide_pos(side).unwrap_or(((row - top) % rows) as f32);
                #[allow(clippy::cast_precision_loss, reason = "строки страницы")]
                let to = ((now.2 - top) % rows) as f32;
                self.slide[i] = Some(Slide {
                    from,
                    to,
                    at: std::time::Instant::now(),
                });
            } else if self.slot[i].is_some_and(|o| o != now) {
                self.slide[i] = None;
            }
            self.slot[i] = Some(now);
            if self.slide[i].is_some_and(|sl| sl.at.elapsed() >= SLIDE) {
                self.slide[i] = None;
            }
            if self.slide[i].is_some() {
                window.request_animation_frame();
            }
        }
    }
}

impl Render for CoreHost {
    #[allow(clippy::too_many_lines, reason = "сборка зон окна")]
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t0 = std::time::Instant::now();
        if tracing::enabled!(target: "urfin::frame", tracing::Level::TRACE)
            && let Some(core) = &self.core
        {
            let t = core.state().active_tab();
            tracing::trace!(target: "urfin::frame", row = t.cursor.row, top = t.cursor.top, "render");
        }
        self.sync_field(window, cx);
        self.sync_feature_inputs(window, cx);
        // Размер окна мог смениться без события границ (или до него): сетка панели — по
        // кадру, иначе PgDn уводит курсор за видимый край.
        self.sync_viewport(window);
        self.sync_inbox(window);
        self.sync_theme(window);
        self.track_slide(window);
        self.drive_bench(window, cx);
        self.drive_screens(window, cx);
        if let Some(f) = self.first_frame.take() {
            // Кадр уже на экране к следующему циклу: замер и, для `--bench-startup`, выход.
            // Следующий цикл просим сразу: кадры по запросу, иначе он придёт, только когда
            // что-то изменится (замер выходил ~1 с вместо ~0,2).
            window.request_animation_frame();
            cx.on_next_frame(window, move |h, _, cx| {
                if f() {
                    h.shutdown(cx);
                }
            });
        }
        let i18n = urfin_i18n::current();
        self.keybar.refresh(
            &i18n,
            urfin_i18n::epoch(),
            &self.registered,
            &crate::keybar::FarStub,
        );
        let mut root = div()
            .track_focus(&self.focus)
            .on_key_up(cx.listener(Self::key_up))
            .on_modifiers_changed(cx.listener(Self::modifiers))
            .size_full()
            .flex()
            .flex_col()
            .bg(self.theme.c(Role::BgBase))
            .text_color(self.theme.c(Role::TextPrimary))
            .text_size(px(self.theme.m(Metric::TypeBody)));
        let Some(core) = &self.core else {
            return root;
        };
        let s = core.state();
        // Дополнение поля (№70) — до обработчиков кликов (им нужен `cx`).
        let completion_el = self.completion_list(window, cx);
        // История Alt+F8 и варианты Tab над cmdline (S6-16, S6-17).
        let term_menu_el = term_menu::view(s, &self.theme, window);
        let conflict_el = self.conflict_list(cx);
        let click = |cmd: Command| -> overlays::Handler {
            Box::new(cx.listener(move |h: &mut CoreHost, _: &ClickEvent, w, cx| {
                h.run(cmd.clone(), w, cx);
            }))
        };
        let click_all = |cmds: Vec<Command>| -> overlays::Handler {
            Box::new(cx.listener(move |h: &mut CoreHost, _: &ClickEvent, w, cx| {
                h.run_all(cmds.clone(), w, cx);
            }))
        };
        // Маска открытой строки отметки группы: совпавшие строки уже показаны обычной
        // отметкой (Num +) или без неё (Num −), Esc откатывает (D-04, `screen-select`).
        self.mask_preview = s
            .ext
            .get::<crate::UiState>()
            .and_then(|u| u.draft.as_ref())
            .filter(|d| matches!(d.cmd.as_str(), "sel.add" | "sel.remove"))
            .map(|d| d.cmd == "sel.add")
            .zip(self.field.as_ref())
            .map(|(add, f)| (f.input.read(cx).value().to_string(), add));
        if let Some(c) = self.features.get_mut::<compare::Compare>() {
            c.life(s);
        }
        self.notes = self.compute_notes(s);
        self.strip_rows = s
            .ext
            .get::<crate::UiState>()
            .and_then(|u| u.draft.as_ref())
            .filter(|d| d.cmd == "ops.mkdir")
            .map_or(0, |_| {
                let text = self
                    .field
                    .as_ref()
                    .map(|f| f.input.read(cx).value().to_string())
                    .unwrap_or_default();
                field::mkdir_tree(s.active_tab(), self.strip_opts.many, &text).len()
            });
        let notes_hash = [0, 1].map(|i| {
            use std::hash::{Hash, Hasher};
            let mut h = std::hash::DefaultHasher::new();
            let mut v: Vec<_> = self.notes[i].iter().collect();
            v.sort_by(|a, b| a.0.cmp(b.0));
            v.hash(&mut h);
            h.finish()
        });
        let drives_open = s
            .ext
            .get::<crate::UiState>()
            .and_then(|u| u.drives.as_ref());
        if drives_open.is_none() {
            self.drive_sel = None;
        }
        // Меню F9; список дисков — свой (не строка меню в заголовке).
        let open = s
            .ext
            .get::<crate::UiState>()
            .and_then(crate::menu::open_id)
            .filter(|id| *id != "drives")
            .map(str::to_owned);
        if open.as_deref() != Some("context") {
            self.context_at = None;
        }
        let dark = self.theme.generation();
        let widths = Self::layout(&self.theme, s, f32::from(window.viewport_size().width));
        // Ширина наборов табов — как в заголовке (`chrome::panel_tabs`), для «› N» (№67).
        self.menu.tabs_avail = chrome::tabs_avail(widths);
        // Ширина текста для корешков меню (D-05).
        let font = window.text_style().font();
        let text_system = window.text_system().clone();
        let measure = move |text: &str, size: f32| -> f32 {
            let run = gpui_kit::TextRun {
                len: text.len(),
                font: font.clone(),
                color: gpui_kit::Hsla::default(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            f32::from(
                text_system
                    .shape_line(SharedString::from(text.to_owned()), px(size), &[run], None)
                    .width,
            )
        };
        // Строка меню F9 — поверх путей панелей; табы остаются (D-05).
        let bar = open
            .as_deref()
            .filter(|id| menu::Kind::of(id, s.active) == menu::Kind::Bar)
            .map(|id| self.menu.bar(id, &self.theme, &click, &measure))
            // Строка и список меню перекрывают щит: клик по пункту не закрывает меню
            // щитом поверх команды пункта.
            .map(|(row, at)| (row.occlude(), at));
        let (bar_row, bar_at) = bar.map_or((None, Vec::new()), |(r, at)| (Some(r), at));
        // Табы — без кэша: в их пустом месте зона перетаскивания окна, а кэш gpui
        // `WindowControlArea` не повторяет.
        let content = if s
            .ext
            .get::<crate::UiState>()
            .is_some_and(|u| u.settings.is_some())
        {
            chrome::settings_tab(&self.theme, &click)
        } else if let Some(tabs) = self
            .features
            .iter()
            .find_map(|f| f.tabs(s, &self.theme, &click))
        {
            // Вкладки экранов (вьювер, S7 §3.9).
            tabs
        } else {
            chrome::panel_tabs(s, &self.theme, &click, widths)
        };
        // Контекст (T-01-5): полоса 3 px поверх верха окна и метка у кнопок; не отключается.
        let badge = self.context.map(|ctx| {
            let (role, key) = match ctx {
                crate::sys::Context::Admin => (Role::ContextAdmin, "ui-context-admin"),
                crate::sys::Context::Remote => (Role::ContextRemote, "ui-context-remote"),
            };
            (self.theme.c(role), i18n.get(key))
        });
        root = root.child(chrome::titlebar(
            content,
            &self.theme,
            window.is_maximized(),
            badge.clone(),
        ));
        if let Some((color, _)) = &badge {
            root = root.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(CONTEXT_BAR))
                    .bg(*color),
            );
        }
        // Панели во всю ширину; фон — цвет линии, поэтому между панелями видна линия 1 px.
        let mut panels = div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_row()
            .bg(self.theme.c(Role::StrokeDivider));
        for (i, side) in [Side::Left, Side::Right].into_iter().enumerate() {
            let Some(w) = widths[i] else {
                continue;
            };
            if i == 1 && widths[0].is_some() {
                // Линия между панелями; ухватиться — за полосу поверх неё: курсор ↔,
                // двойной клик — 50/50 (S2-07).
                panels = panels.child(
                    div()
                        .w(px(self.theme.m(Metric::WDivider)))
                        .flex_none()
                        .h_full()
                        .relative()
                        .child(
                            div()
                                .id("splitter")
                                .absolute()
                                .top_0()
                                .left(px(-SPLITTER_GRAB))
                                .w(px(2.0 * SPLITTER_GRAB + 1.0))
                                .h_full()
                                .cursor(gpui_kit::CursorStyle::ResizeLeftRight)
                                .on_click(cx.listener(|h, ev: &ClickEvent, w, cx| {
                                    if ev.click_count() >= 2 {
                                        h.run(Command::new("ui.split").arg("reset", true), w, cx);
                                    }
                                })),
                        ),
                );
            }
            // Изменилась — рисуется заново (без кэша в этом кадре), иначе кадр из кэша.
            let t = s.panel(side).tab();
            let files = t.mode == urfin_core::PanelMode::Files;
            let mut card = panel::card(&self.theme);
            // Быстрый просмотр и вьювер `viewer.dock` — тело панели рисует фича вьювера
            // (S7 §4): её кэш строк, мышь, документ Markdown.
            if view::panel_shows(s, side) && view::shown(s).is_none() {
                let frame = features::Frame {
                    s,
                    theme: &self.theme,
                    click: &click,
                    click_all: &click_all,
                };
                if let Some(el) = view::panel_body(&mut self.features, &frame, side, w, window, cx)
                {
                    self.prints[i] = [0; 3];
                    card = card.child(div().flex_1().min_h_0().flex().child(el));
                    panels = panels.child(div().w(px(w)).flex_none().h_full().flex().child(card));
                    continue;
                }
            }
            for (k, piece) in [panel::Piece::Head, panel::Piece::List]
                .into_iter()
                .enumerate()
            {
                let slot = match piece {
                    panel::Piece::Head if files => div().flex_none().h(px(panel::head_h(
                        &self.theme,
                        t.view_mode,
                        panel::state_of(t).is_some(),
                    ))),
                    panel::Piece::List => div().flex_1().min_h_0(),
                    _ => continue,
                };
                // Каталог грузится (до `HOLD_LOADING`): прежний кадр панели, как в Far —
                // без пустого кадра и перестановок строк, пока подходят порции.
                if let urfin_core::ListingStatus::Loading { since } = t.listing.status
                    && since.elapsed() < HOLD_LOADING
                    && self.prints[i][k] != 0
                {
                    window.request_animation_frame();
                    let held = self.prints[i][k];
                    card = card.child(slot.flex().child(cached(
                        &self.panels[i][k],
                        held,
                        &mut self.prints[i][k],
                    )));
                    continue;
                }
                let anim = self.slide[i].map_or(0, |sl| sl.at.elapsed().as_micros() as u64 + 1);
                let print = panel::fingerprint(
                    s,
                    side,
                    piece,
                    self.grid[i],
                    dark,
                    self.field.as_ref(),
                    anim ^ self
                        .mask_preview
                        .as_ref()
                        .map_or(0, |(m, add)| fxhash(m) ^ u64::from(*add))
                        ^ notes_hash[i],
                );
                card = card.child(slot.flex().child(cached(
                    &self.panels[i][k],
                    print,
                    &mut self.prints[i][k],
                )));
            }
            panels = panels.child(div().w(px(w)).flex_none().h_full().flex().child(card));
        }
        // Встроенная строка (D-04): вопрос, черновик или быстрый поиск — поверх низа
        // панелей, на ширину активной панели или на всю ширину.
        let field_text = self
            .field
            .as_ref()
            .filter(|f| matches!(f.key, field::Key::Draft(_)))
            .map(|f| f.input.read(cx).value().to_string())
            .unwrap_or_default();
        // Опции строк: 0 — список «Если файл есть», 1 — хэш, 2 — несколько папок, 10…13 —
        // атрибуты, 20 — «сейчас» в поле времени.
        let toggle = |what: u8| -> overlays::Handler {
            Box::new(
                cx.listener(move |h: &mut CoreHost, _: &ClickEvent, window, cx| {
                    let o = &mut h.strip_opts;
                    match what {
                        0 => o.dropdown = !o.dropdown,
                        1 => o.verify = !o.verify,
                        2 => o.many = !o.many,
                        10..=13 => {
                            let k = usize::from(what - 10);
                            o.attrs[k] = field::next_attr(o.attrs_now[k], o.attrs[k]);
                        }
                        40..=42 => o.link_kind = usize::from(what - 40),
                        30..=34 => {
                            let token = field::MR_TOKENS[usize::from(what - 30)].0;
                            if let Some(f) = h.field.as_ref() {
                                f.input.update(cx, |s, cx| {
                                    let v = format!("{}{token}", s.value());
                                    s.set_value(v, window, cx);
                                });
                            }
                        }
                        _ => {
                            let now = h.now_iso();
                            if let Some(f) = h.field.as_ref() {
                                f.input.update(cx, |s, cx| s.set_value(now, window, cx));
                            }
                        }
                    }
                    cx.notify();
                }),
            )
        };
        let frame = features::Frame {
            s,
            theme: &self.theme,
            click: &click,
            click_all: &click_all,
        };
        let strip_el = if let Some(p) = s.prompts.active() {
            let sel = self.choice.of(p);
            let toggle_all: overlays::Handler =
                Box::new(cx.listener(move |h: &mut CoreHost, _: &ClickEvent, _, cx| {
                    h.choice = sel;
                    h.choice.all = !sel.all;
                    cx.notify();
                }));
            Some(overlays::prompt_bar(
                p,
                sel,
                &self.theme,
                &click,
                toggle_all,
            ))
        } else {
            field::draft_bar(
                s,
                self.field.as_ref(),
                &field_text,
                self.ops_permanent,
                &self.strip_opts,
                self.mr_preview.as_ref(),
                self.features
                    .get::<compare::Compare>()
                    .and_then(compare::Compare::sync),
                &self.theme,
                &click_all,
                &toggle,
            )
            .or_else(|| strip::quick(s, &self.theme))
            .or_else(|| self.features.iter().find_map(|f| f.strip(&frame, cx)))
        };
        let cover = strip::cover(
            s,
            self.strip_rows,
            self.features
                .get::<compare::Compare>()
                .is_some_and(|c| c.mode(s)),
        );
        // Тосты справа — над встроенной строкой, если она накрывает правую сторону.
        let toast_lift = cover
            .filter(|(scope, _)| scope.covers(Side::Right))
            .map_or(0.0, |(_, h)| h);
        for el in self.features.iter().filter_map(|f| f.overlay(&frame, cx)) {
            panels = panels
                .relative()
                .child(div().absolute().bottom_0().left_0().right_0().child(el));
        }
        if let (Some(el), Some((scope, _))) = (strip_el, cover) {
            let (x, w) = scope.bounds(widths, self.theme.m(Metric::WDivider));
            panels = panels
                .relative()
                .child(div().absolute().bottom_0().left(px(x)).w(px(w)).child(el));
        }
        // Зона событий внизу (№63), снизу вверх: keybar, статус, командная строка, job-бар,
        // строка-вопрос или баннер; тосты — справа над ней.
        // Область панелей у фич: вкладка настроек (D-08) — вместо панелей; панели
        // спрятаны (Ctrl+O) — поверх них сетка терминала (S6-06).
        let hidden = widths == [None, None];
        let (mut cover_el, mut replace_el) = (None, None);
        for f in self.features.iter_mut() {
            match f.main(&frame, hidden, window, cx) {
                Some(features::Main::Replace(el)) => replace_el = replace_el.or(Some(el)),
                Some(features::Main::Cover(el)) => cover_el = cover_el.or(Some(el)),
                None => {}
            }
        }
        let main = match (replace_el, cover_el) {
            (Some(el), _) => div().flex_1().min_h_0().child(el),
            (None, Some(el)) => panels.relative().child(el),
            (None, None) => panels,
        };
        let narrow = f32::from(window.viewport_size().width) < chrome::NARROW_W;
        let root = root
            .child(main)
            .children(completion_el)
            .children(term_menu_el)
            .children(conflict_el)
            .children(overlays::toasts(&self.toasts, &self.theme, toast_lift))
            .children(overlays::banner(s, &self.theme, &click))
            .children(self.features.iter().filter_map(|f| f.bar(&frame)))
            .children(
                chrome::Bottom::ALL
                    .into_iter()
                    .enumerate()
                    .map(|(k, part)| {
                        let print = chrome::bottom_print(
                            s,
                            part,
                            self.keybar.items(self.mods),
                            self.mods,
                            dark,
                            narrow,
                            &self.theme.fmt,
                        );
                        div()
                            .flex_none()
                            .h(px(part.h(s, &self.theme)))
                            .flex()
                            .child(cached(&self.bottom[k], print, &mut self.bottom_print[k]))
                    }),
            )
            .children(open.as_ref().map(|_| {
                // Щит под меню: клик мимо закрывает его и не уходит в панель.
                div()
                    .id("menu-shield")
                    .absolute()
                    .top(px(self.theme.m(Metric::HTitlebar)))
                    .left_0()
                    .size_full()
                    // Панели, строка ввода и keybar под щитом мышь не получают.
                    .occlude()
                    .on_click(cx.listener(|h, _: &ClickEvent, w, cx| {
                        h.run(Command::new("ui.cancel"), w, cx);
                    }))
            }))
            .children(bar_row)
            .children(open.as_ref().and_then(|id| {
                let geo = menu::Geo {
                    win_w: f32::from(window.viewport_size().width),
                    win_h: f32::from(window.viewport_size().height),
                    top: self.theme.m(Metric::HTitlebar),
                    path_h: self.theme.m(Metric::HBreadcrumb),
                    head_h: self.theme.m(Metric::HColHeader),
                    row_h: self.theme.m(Metric::HRow),
                    panels: Self::panel_spans(&self.theme, widths),
                    pointer: self.context_at,
                };
                self.menu
                    .flyout(
                        id,
                        s,
                        &crate::keybar::FarStub,
                        &self.theme,
                        &click_all,
                        &geo,
                        &bar_at,
                        &measure,
                    )
                    .map(InteractiveElement::occlude)
            }))
            .children(drives_open.map(|d| {
                let x = if d.side == Side::Right {
                    widths[0].map_or(0.0, |w| w + self.theme.m(Metric::WDivider))
                } else {
                    0.0
                };
                div()
                    .id("drives-shield")
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .occlude()
                    .on_click(cx.listener(|h, _: &ClickEvent, w, cx| {
                        h.run(Command::new("ui.cancel"), w, cx);
                    }))
                    .child(
                        drives::flyout(
                            d,
                            drives::selected(self.drive_sel, d),
                            x,
                            &self.theme,
                            &click_all,
                        )
                        .occlude(),
                    )
            }));
        if let Some(b) = self.bench.as_mut() {
            b.built(t0.elapsed());
        }
        root
    }
}

/// Хэш строки для отпечатка видов (маска отметки группы).
fn fxhash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Полоса контекста сверху окна (T-01-5), px.
const CONTEXT_BAR: f32 = 3.0;

/// Полоса захвата сплиттера по обе стороны линии, px.
/// Ширина списка дополнений не меньше.
const COMPLETE_W: f32 = 360.0;
/// Ширина списка «Если файл есть».
const CONFLICT_W: f32 = 240.0;
const SPLITTER_GRAB: f32 = 3.0;

/// Окно GPUI с Core; возвращается после выхода из цикла (сессия уже сохранена).
pub fn run(launch: Launch, geom: Option<WindowGeom>) {
    let t0 = std::time::Instant::now();
    let app = gpui_kit::application();
    tracing::debug!(target: "startup", ms = t0.elapsed().as_secs_f64() * 1e3, "gpui application");
    app.run(move |cx: &mut App| {
        tracing::debug!(target: "startup", ms = t0.elapsed().as_secs_f64() * 1e3, "gpui run");
        gpui_kit::init(cx);
        tracing::debug!(target: "startup", ms = t0.elapsed().as_secs_f64() * 1e3, "gpui-kit init");
        // `--bench` не мешает владельцу: окно на неосновном экране (ноутбук при втором
        // мониторе), без фокуса и без ограничения кадров неактивного окна.
        let bench = launch.bench.is_some() || launch.screens.is_some();
        let bench_display = bench
            .then(|| {
                let primary = cx.primary_display().map(|d| d.id());
                cx.displays()
                    .into_iter()
                    .map(|d| d.id())
                    .find(|&id| Some(id) != primary)
            })
            .flatten();
        let geom = if bench { None } else { geom };
        let bounds = match geom {
            Some(g) => Bounds {
                origin: point(px(g.x), px(g.y)),
                size: size(px(g.w), px(g.h)),
            },
            None => Bounds::centered(bench_display, size(px(1200.), px(760.)), cx),
        };
        let window_bounds = if geom.is_some_and(|g| g.maximized) {
            WindowBounds::Maximized(bounds)
        } else {
            WindowBounds::Windowed(bounds)
        };
        let options = WindowOptions {
            window_bounds: Some(window_bounds),
            // Свой заголовок с табами (S2 §1.4): системный скрыт.
            titlebar: Some(TitlebarOptions {
                title: Some(SharedString::new_static("Urfin")),
                appears_transparent: true,
                ..Default::default()
            }),
            focus: !bench,
            display_id: bench_display,
            // gpui по умолчанию держит неактивное окно на 30 кадр/с. Урфин рисует только по
            // изменениям (в простое 0 кадров), а потолок рвёт ритм кадров при автоповторе
            // клавиш, если gpui сочтёт окно неактивным.
            inactive_frame_interval: None,
            ..Default::default()
        };
        let opened = gpui_kit::open_window(options, cx, move |window, cx| {
            tracing::debug!(target: "startup", ms = t0.elapsed().as_secs_f64() * 1e3, "window created");
            let host = cx.new(|cx| CoreHost::new(launch, cx));
            host.update(cx, |h, cx| {
                window.focus(&h.focus, cx);
                // Записки клавиш, которые gpui теряет (S3-07): subclass окна пишет их в
                // очередь конвейера keymap.
                #[cfg(windows)]
                {
                    let signal = Arc::clone(&h.signal);
                    let wake = Box::new(move || signal.keys());
                    if !crate::sys::key_notes::install(window, h.input.notes(), wake) {
                        tracing::warn!("key notes: window subclass not installed");
                    }
                }
                // Крестик окна — через `app.quit`: handlers могут наложить вето (S0 v2 №13).
                let weak = cx.weak_entity();
                window.on_window_should_close(cx, move |window, cx| {
                    weak.update(cx, |h, cx| {
                        if h.core.is_none() {
                            return true;
                        }
                        // Перемещение окна bounds-наблюдатель не видит: позиция — на выходе.
                        h.track_bounds(window);
                        h.dispatch("app.quit", window, cx);
                        h.core.is_none()
                    })
                    .unwrap_or(true)
                });
                cx.observe_window_activation(window, CoreHost::activation)
                    .detach();
                cx.observe_window_appearance(window, |_, _, cx| cx.notify())
                    .detach();
                cx.observe_window_bounds(window, |h, window, cx| {
                    h.track_bounds(window);
                    h.sync_viewport(window);
                    h.pump(window, cx);
                })
                .detach();
                h.sync_viewport(window);
                h.pump(window, cx);
            });
            // Клавиши — до привязок gpui-kit (его `Root` забирает Tab на переход фокуса).
            let weak = host.downgrade();
            cx.intercept_keystrokes(move |ev, window, cx| {
                // Журнал кадров (`[log] level = "info,urfin::frame=trace"`): нажатие → render.
                tracing::trace!(target: "urfin::frame", key = %ev.keystroke.key, "key");
                let consumed = weak
                    .update(cx, |h, cx| h.key_down(&ev.keystroke, window, cx))
                    .unwrap_or(false);
                if consumed {
                    cx.stop_propagation();
                }
            })
            .detach();
            host
        });
        tracing::debug!(target: "startup", ms = t0.elapsed().as_secs_f64() * 1e3, "open_window returned");
        if let Err(e) = opened {
            tracing::error!(error = %e, "gpui window");
            cx.quit();
        }
        cx.activate(true);
    });
}

/// Win32-дескриптор окна gpui как `isize` (0 — не Win32).
fn hwnd_of(window: &Window) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match HasWindowHandle::window_handle(window).map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
        _ => 0,
    }
}
