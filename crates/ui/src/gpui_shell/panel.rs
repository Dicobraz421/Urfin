//! Панель файлов на GPUI (G-04, S2 §2.1–2.2): карточка с путём, шапкой колонок,
//! строками 24 px и подвалом. Рисуется только видимая страница от `cursor.top` —
//! её держит Core по `panel.viewport` (S0 v3 №3).

use gpui_kit::prelude::*;
use std::rc::Rc;

use gpui_kit::{
    App, Bounds, ClickEvent, ContentMask, Context, Div, Font, Hsla, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollWheelEvent,
    SharedString, Stateful, TextAlign, TextRun, TruncateFrom, WeakEntity, Window, canvas, div,
    point, px, size as size2,
};
use urfin_core::{AppState, Row, Side, Tab, ViewMode};
use urfin_i18n::FluentArgs;
use urfin_vfs::icons::icon_class;

use super::CoreHost;
use super::theme::Theme;
use crate::config::IconMode;
use crate::fmt_cell::{Cell, ColKind, cell_text};
use crate::tokens::{Metric, Role};

/// Ширина колонок Full: размер и дата (S2 §2.1).
const W_SIZE: f32 = 92.0;
const W_MTIME: f32 = 120.0;
/// Минимальная ширина колонки Brief.
/// Brief — только имена в три колонки (D-03).
const BRIEF_COLS: f32 = 3.0;
/// Шрифт иконок ОС (S2 §1.6).
const ICON_FONT: &str = "Segoe Fluent Icons";
/// `..` — стрелка вверх.
const GLYPH_UP: char = '\u{E74A}';

/// Геометрия панели для `panel.viewport`: строк в колонке и колонок.
pub(super) fn grid(theme: &Theme, mode: ViewMode, _w: f32, h: f32) -> (u32, u32) {
    let head = if matches!(mode, ViewMode::Full) {
        theme.m(Metric::HColHeader)
    } else {
        0.0
    };
    let list_h = h - theme.m(Metric::HBreadcrumb) - head;
    #[allow(clippy::cast_sign_loss, reason = "не меньше 1.0")]
    let rows = (list_h / theme.m(Metric::HRow)).floor().max(1.0) as u32;
    let cols = match mode {
        ViewMode::Brief => BRIEF_COLS,
        ViewMode::Medium => 2.0,
        _ => 1.0,
    };
    #[allow(clippy::cast_sign_loss, reason = "не меньше 1.0")]
    (rows, cols as u32)
}

/// Обработчики событий панели: замыкания над хостом (как `cx.listener`, но панель —
/// своя view и рисуется вне `Context<CoreHost>`).
#[derive(Clone)]
pub(super) struct Listen(pub WeakEntity<CoreHost>);

impl Listen {
    pub fn listener<E: ?Sized>(
        &self,
        f: impl Fn(&mut CoreHost, &E, &mut Window, &mut Context<CoreHost>) + 'static,
    ) -> impl Fn(&E, &mut Window, &mut App) + 'static {
        let host = self.0.clone();
        move |e, window, cx| {
            let _ = host.update(cx, |h, cx| f(h, e, window, cx));
        }
    }
}

/// Часть панели — своя view: окно кладёт её кэшированной (`cached`), и кадр, где она не
/// менялась, gpui повторяет без раскладки и отрисовки (G-07). При листании меняется
/// только список; путь, шапка колонок и подвал берутся из кэша.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Piece {
    /// Путь и шапка колонок.
    Head,
    /// Строки (или дерево, «Сведения», просмотр).
    List,
    /// Итоги, быстрый поиск, аккорд.
    Foot,
}

pub(super) struct PanelView {
    pub host: WeakEntity<CoreHost>,
    pub side: Side,
    pub piece: Piece,
}

impl Render for PanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(host) = self.host.upgrade() else {
            return div();
        };
        let h = host.read(cx);
        let Some(core) = h.core.as_ref() else {
            return div();
        };
        let s = core.state();
        let side = self.side;
        let t = s.panel(side).tab();
        let theme = &h.theme;
        let l = Listen(self.host.clone());
        let (rows, cols) = h.grid[usize::from(side == Side::Right)];
        let body = div().size_full().flex().flex_col();
        match self.piece {
            Piece::Head => {
                let panel_w = h.panel_w[usize::from(side == Side::Right)];
                let font_px = px(theme.m(Metric::TypeBody));
                let font = window.text_style().font();
                let measure = |text: &str| -> f32 {
                    let run = TextRun {
                        len: text.len(),
                        font: font.clone(),
                        color: Hsla::default(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    };
                    f32::from(
                        window
                            .text_system()
                            .shape_line(SharedString::from(text.to_owned()), font_px, &[run], None)
                            .width,
                    )
                };
                let cols = fit_cols(theme, t.view_mode, panel_w);
                body.child(path_bar(
                    t,
                    side,
                    s.active == side,
                    theme,
                    h.field.as_ref(),
                    panel_w,
                    &measure,
                    &l,
                ))
                .when(
                    matches!(t.view_mode, ViewMode::Full) && state_of(t).is_none(),
                    |d| d.child(col_header(side, theme, cols, &l)),
                )
            }
            // Дерево, «Сведения», просмотр (S2 §2.4–2.5).
            Piece::List if t.mode != urfin_core::PanelMode::Files => {
                body.child(super::views::body(s, side, theme, rows))
            }
            Piece::List if state_of(t).is_some() => body.child(state_view(t, side, theme, &l)),
            Piece::List => body.child(list(
                t,
                side,
                s.active == side,
                theme,
                rows,
                cols,
                h.field.as_ref(),
                h.slide_pos(side),
                s.ext
                    .get::<crate::UiState>()
                    .map_or(IconMode::Mono, |u| u.icons),
                &Highlight {
                    prefix: s
                        .ext
                        .get::<urfin_keymap::KeymapView>()
                        .and_then(|k| k.qs.as_ref())
                        .filter(|q| q.side == side)
                        .map(|q| q.mask.to_string()),
                    mask: h
                        .mask_preview
                        .as_ref()
                        .filter(|_| s.active == side)
                        .and_then(|(m, add)| {
                            urfin_core::Mask::parse(m.trim()).ok().map(|m| (m, *add))
                        }),
                    notes: std::rc::Rc::clone(&h.notes[usize::from(side == Side::Right)]),
                    links: s
                        .ext
                        .get::<crate::UiState>()
                        .map(|u| u.links[usize::from(side == Side::Right)].clone())
                        .unwrap_or_default(),
                },
                &l,
            )),
            Piece::Foot => body,
        }
    }
}

/// Что подсветить в именах списка (D-04).
#[derive(Clone, Debug, Default)]
pub(super) struct Highlight {
    /// Набранное в быстром поиске этой панели.
    pub prefix: Option<String>,
    /// Маска открытой строки отметки группы и что она делает: отметить (`true`) или
    /// снять отметку.
    pub mask: Option<(urfin_core::Mask, bool)>,
    /// Заметки после имени по имени элемента (D-04): превью переименования, метки
    /// сравнения, действие плана синхронизации.
    pub notes: std::rc::Rc<std::collections::HashMap<String, Note>>,
    /// Ссылки каталога (D-03): вид и «битая».
    pub links: crate::state::Links,
}

/// Заметка после имени в строке списка.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Note {
    /// Групповое переименование: «→ новое имя» (`accent`, проблема — `modified`).
    Rename { to: String, bad: bool },
    /// Метка сравнения: «новее», «только здесь».
    Tag(String),
    /// Действие плана синхронизации: «→», «←» в рамке `accent`.
    Arrow(&'static str),
}

/// Отпечаток всего, что рисует часть панели: изменился — часть рисуется заново, иначе
/// кадр берётся из кэша gpui. Ввод в поле внутри части помечает её сам (предок поля).
pub(super) fn fingerprint(
    s: &AppState,
    side: Side,
    piece: Piece,
    grid: (u32, u32),
    look: u64,
    field: Option<&super::field::Field>,
    anim: u64,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    let t = s.panel(side).tab();
    let active = s.active == side;
    (side == Side::Right, look, urfin_i18n::epoch(), t.id.0).hash(&mut h);
    let icons = s.ext.get::<crate::UiState>().map(|u| u.icons as u8);
    icons.hash(&mut h);
    format!("{:?}{:?}", t.view_mode, t.mode).hash(&mut h);
    if let Some(f) = field {
        format!("{:?}{}", f.key, f.input.entity_id()).hash(&mut h);
    }
    match piece {
        Piece::Head => {
            format!("{:?}", t.loc).hash(&mut h);
            t.filter.as_ref().map(urfin_core::Mask::as_str).hash(&mut h);
            (t.listing.order.len(), t.listing.entries.len()).hash(&mut h);
        }
        Piece::List => {
            (
                active,
                grid,
                t.rev,
                t.cursor.row,
                t.cursor.top,
                t.rows(),
                anim,
            )
                .hash(&mut h);
            (t.selection.len(), t.selection.bytes()).hash(&mut h);
            if let Some(u) = s.ext.get::<crate::UiState>() {
                u.links[usize::from(side == Side::Right)]
                    .items
                    .len()
                    .hash(&mut h);
            }
            // Подсветка префикса быстрого поиска (D-04).
            if let Some(q) = s
                .ext
                .get::<urfin_keymap::KeymapView>()
                .and_then(|k| k.qs.as_ref())
                .filter(|q| q.side == side)
            {
                q.mask.hash(&mut h);
            }
            if t.mode != urfin_core::PanelMode::Files {
                // Дерево, «Сведения», просмотр смотрят и на другую панель.
                s.rev.hash(&mut h);
            }
        }
        Piece::Foot => {
            (active, t.rev, t.selection.len(), t.selection.bytes()).hash(&mut h);
            if let Some(k) = s.ext.get::<urfin_keymap::KeymapView>() {
                if let Some(q) = k.qs.as_ref().filter(|q| q.side == side) {
                    format!("{q:?}").hash(&mut h);
                }
                if active {
                    k.pending.hash(&mut h);
                }
            }
        }
    }
    h.finish()
}

/// Эксперимент с курсором (`URFIN_CURSOR=solid,slide`, выключен по умолчанию): сплошной
/// акцентный курсор и скольжение полосы между строками. Решение — за владельцем.
pub(super) fn cursor_mode() -> (bool, bool) {
    static M: std::sync::OnceLock<(bool, bool)> = std::sync::OnceLock::new();
    *M.get_or_init(|| {
        let v = std::env::var("URFIN_CURSOR").unwrap_or_default();
        (v.contains("solid"), v.contains("slide"))
    })
}

/// Панель — плоская, фон `panel`, без рамки (D-02): активную видно по подчёркнутому
/// пути. Части кладёт окно.
pub(super) fn card(theme: &Theme) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(theme.c(Role::LayerPanel))
}

/// Состояние папки вместо списка (D-03, `screen-errors.png`, `screen-archive.png`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum FolderState {
    Empty,
    /// Нет доступа: замок `modified`, «Назад».
    Denied,
    /// Нет связи: `context_remote`, «Повторить сейчас».
    Offline,
    /// Прочая ошибка чтения — её текст.
    Failed(String),
}

/// Что показать вместо списка; `None` — список.
pub(super) fn state_of(t: &Tab) -> Option<FolderState> {
    match &t.listing.status {
        urfin_core::ListingStatus::Complete if t.listing.order.is_empty() => {
            Some(FolderState::Empty)
        }
        urfin_core::ListingStatus::Failed(e) => Some(if e.starts_with("access denied") {
            FolderState::Denied
        } else if is_offline(e) || t.loc.path.as_os_str().to_string_lossy().starts_with(r"\\") {
            FolderState::Offline
        } else {
            FolderState::Failed(e.to_string())
        }),
        _ => None,
    }
}

/// Ошибки сети Windows: путь не найден, имя недоступно, сеть отключена.
fn is_offline(e: &str) -> bool {
    [53, 64, 67, 1222, 1231, 1232, 1311]
        .iter()
        .any(|c| e.contains(&format!("(os error {c})")))
}

/// Вид состояния: значок 56 в круге, заголовок 15/500, пояснение `secondary` до 380,
/// кнопки с клавишами. Путь — только в заголовке панели.
/// Вид состояния: значок, цвет, заголовок, пояснение, кнопки (ключ Fluent, клавиша, команда).
type StateLook = (
    char,
    Role,
    String,
    String,
    Vec<(&'static str, &'static str, urfin_core::Command)>,
);

#[allow(clippy::too_many_lines, reason = "значок, текст и кнопки сверху вниз")]
fn state_view(t: &Tab, side: Side, theme: &Theme, cx: &Listen) -> Div {
    let i18n = urfin_i18n::current();
    let Some(st) = state_of(t) else {
        return div();
    };
    let mut args = FluentArgs::new();
    args.set("path", t.loc.path.display().to_string());
    let (glyph, color, title, text, buttons): StateLook = match &st {
        FolderState::Empty => (
            '\u{E8B7}',
            Role::TextSecondary,
            i18n.get("ui-state-empty"),
            String::new(),
            Vec::new(),
        ),
        FolderState::Denied => (
            '\u{E72E}',
            Role::Modified,
            i18n.get("ui-state-denied"),
            i18n.get("ui-state-denied-text"),
            vec![(
                "ui-state-back",
                "Backspace",
                urfin_core::Command::new("panel.up"),
            )],
        ),
        FolderState::Offline => (
            '\u{E968}',
            Role::ContextRemote,
            i18n.fmt("ui-state-offline", &args),
            i18n.get("ui-state-offline-text"),
            vec![(
                "ui-state-retry",
                "Ctrl+R",
                urfin_core::Command::new("panel.refresh"),
            )],
        ),
        FolderState::Failed(e) => (
            '\u{E783}',
            Role::CriticalFg,
            i18n.get("ui-state-failed"),
            e.clone(),
            vec![(
                "ui-state-retry",
                "Ctrl+R",
                urfin_core::Command::new("panel.refresh"),
            )],
        ),
    };
    let name = super::chrome::side_name(side);
    let mut row = div().flex().gap(px(8.0)).mt(px(6.0));
    for (i, (key, hotkey, cmd)) in buttons.into_iter().enumerate() {
        let primary = i == 0;
        row = row.child(
            div()
                .id(SharedString::from(format!("state-{name}-{i}")))
                .h(px(30.0))
                .px(px(14.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(theme.c(if primary {
                    Role::AccentFill
                } else {
                    Role::StrokeControl
                }))
                .bg(theme.c(if primary {
                    Role::AccentFill
                } else {
                    Role::LayerPanel
                }))
                .text_color(theme.c(if primary {
                    Role::AccentOn
                } else {
                    Role::TextPrimary
                }))
                .child(SharedString::from(i18n.get(key)))
                .child(
                    div()
                        .text_size(px(11.0))
                        .opacity(0.8)
                        .child(SharedString::new_static(hotkey)),
                )
                .on_click(cx.listener(move |h, _: &ClickEvent, w, cx| {
                    h.run_all(
                        vec![
                            urfin_core::Command::new("panel.focus").arg("side", name),
                            cmd.clone(),
                        ],
                        w,
                        cx,
                    );
                })),
        );
    }
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(10.0))
        .px(px(40.0))
        .pb(px(40.0))
        .child(
            div()
                .size(px(56.0))
                .rounded(px(28.0))
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.c(Role::Header))
                .border_1()
                .border_color(theme.c(Role::StrokeDivider))
                .font_family(ICON_FONT)
                .text_size(px(24.0))
                .text_color(theme.c(color))
                .child(SharedString::from(glyph.to_string())),
        )
        .child(
            div()
                .text_size(px(15.0))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(SharedString::from(title)),
        )
        .when(!text.is_empty(), |d| {
            d.child(
                div()
                    .max_w(px(380.0))
                    .text_center()
                    .text_color(theme.c(Role::TextSecondary))
                    .child(SharedString::from(text)),
            )
        })
        .child(row)
}

/// Высота шапки панели: путь и, в Full, строка колонок (у состояния папки — без неё).
pub(super) fn head_h(theme: &Theme, mode: ViewMode, state: bool) -> f32 {
    theme.m(Metric::HBreadcrumb)
        + if matches!(mode, ViewMode::Full) && !state {
            theme.m(Metric::HColHeader)
        } else {
            0.0
        }
}

/// Сегменты пути от корня: (подпись, путь до него включительно).
fn crumbs(t: &Tab) -> Vec<(String, std::path::PathBuf)> {
    let mut acc = std::path::PathBuf::new();
    let mut out: Vec<(String, std::path::PathBuf)> = Vec::new();
    for c in t.loc.path.components() {
        acc.push(c.as_os_str());
        let label = match c {
            std::path::Component::Prefix(p) => p.as_os_str().to_string_lossy().into_owned(),
            // Сегмент диска ведёт в корень `C:\`, а не в `C:` (текущий каталог диска).
            std::path::Component::RootDir => {
                if let Some(last) = out.last_mut() {
                    last.1.clone_from(&acc);
                }
                continue;
            }
            other => other.as_os_str().to_string_lossy().into_owned(),
        };
        out.push((label, acc.clone()));
    }
    out
}

/// Путь панели (D-02, S2-12): строка пути `C:\Dev\urfin` — у активной панели цвет
/// `text` и подчёркивание `accent` 2 px, у другой — `secondary`; справа «N объектов».
/// Сегмент — переход к нему, пустое место — правка пути. Не влезает — начальные
/// сегменты сворачиваются в «…» (переход к последнему скрытому), текущая папка видна.
#[allow(clippy::too_many_arguments, reason = "плоский вызов отрисовки")]
#[allow(clippy::too_many_lines, reason = "сегменты слева направо")]
fn path_bar(
    t: &Tab,
    side: Side,
    active: bool,
    theme: &Theme,
    field: Option<&super::field::Field>,
    width: f32,
    measure: &dyn Fn(&str) -> f32,
    cx: &Listen,
) -> Div {
    let bar = div()
        .h(px(theme.m(Metric::HBreadcrumb)))
        .flex_none()
        .flex()
        .items_center()
        .px(px(theme.m(Metric::PadRowX)))
        .text_size(px(PATH_TEXT))
        .border_b_1()
        .border_color(theme.c(Role::StrokeDivider));
    if let Some(input) = super::field::path_input(field, side) {
        return bar.child(input);
    }
    let name = super::chrome::side_name(side);
    let color = theme.c(if active {
        Role::TextPrimary
    } else {
        Role::TextSecondary
    });
    let mut segs = div()
        .flex()
        .items_center()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_color(color)
        .when(active, |d| {
            d.border_b_2()
                .border_color(theme.c(Role::AccentFill))
                .pb(px(3.0))
                .mt(px(5.0))
        });
    let list = if t.loc.is_local() {
        crumbs(t)
    } else {
        Vec::new()
    };
    let first = first_shown(&list, width - 2.0 * theme.m(Metric::PadRowX), measure);
    let sep = || div().child(SharedString::new_static("\\"));
    let seg = |id: String, label: SharedString, path: std::path::PathBuf| {
        let loc = urfin_core::Location::local(path);
        div()
            .id(SharedString::from(id))
            .hover(Styled::underline)
            .child(label)
            .on_click(cx.listener(move |h, _: &ClickEvent, w, cx| {
                h.run(
                    urfin_core::Command::new("panel.go")
                        .arg("side", name)
                        .arg("path", loc.clone()),
                    w,
                    cx,
                );
            }))
    };
    if first > 0 {
        // «…» — к последнему скрытому сегменту.
        let (_, path) = &list[first - 1];
        segs = segs
            .child(seg(
                format!("crumb-{name}-more"),
                SharedString::new_static("…"),
                path.clone(),
            ))
            .child(sep());
    }
    for (k, (label, path)) in list.into_iter().enumerate().skip(first) {
        // У диска разделитель уже в подписи (`C:` + `\`).
        if k > first {
            segs = segs.child(sep());
        }
        segs = segs.child(seg(
            format!("crumb-{name}-{k}"),
            SharedString::from(label),
            path,
        ));
    }
    if !t.loc.is_local() {
        segs = segs.child(SharedString::from(t.loc.to_uri()));
    }
    let i18n = urfin_i18n::current();
    let mut args = FluentArgs::new();
    let st = &t.listing.stats;
    args.set("count", u64::from(st.files + st.dirs));
    // У состояния папки (ошибка, пусто) счётчика нет — путь и вид состояния (D-03).
    // С фильтром — «видно N из M» (D-04, `screen-filtered`).
    let count = if state_of(t).is_some() {
        String::new()
    } else if t.filter.is_some() {
        args.set("count", t.listing.order.len() as u64);
        args.set("total", t.listing.entries.len() as u64);
        i18n.fmt("ui-panel-count-filtered", &args)
    } else {
        i18n.fmt("ui-panel-count", &args)
    };
    let chip = t.filter.as_ref().map(|m| {
        let mut a = FluentArgs::new();
        a.set("mask", m.as_str().to_owned());
        let run = move |cmd: urfin_core::Command| {
            cx.listener(move |h, _: &ClickEvent, w, cx| {
                h.run(
                    urfin_core::Command::new("panel.focus").arg("side", name),
                    w,
                    cx,
                );
                h.run(cmd.clone(), w, cx);
            })
        };
        div()
            .id(SharedString::from(format!("filter-chip-{name}")))
            .flex_none()
            .ml(px(8.0))
            .h(px(20.0))
            .px(px(6.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(theme.c(Role::AccentFill))
            .bg(theme.c(Role::FillCursor))
            .text_size(px(11.5))
            .text_color(theme.c(Role::TextPrimary))
            .child(
                div()
                    .font_family(ICON_FONT)
                    .child(SharedString::new_static("\u{E71C}")),
            )
            .child(SharedString::from(i18n.fmt("ui-filter-chip", &a)))
            .child(
                div()
                    .id(SharedString::from(format!("filter-clear-{name}")))
                    .text_color(theme.c(Role::TextSecondary))
                    .child(SharedString::new_static("×"))
                    .on_click(run(urfin_core::Command::new("panel.filter").arg("mask", ""))),
            )
            .on_click(run(urfin_core::Command::new("panel.filter")))
    });
    bar.child(segs)
        .children(chip)
        .child(
            div()
                .id(SharedString::from(format!("crumb-edit-{name}")))
                .flex_1()
                .h_full()
                .on_click(cx.listener(move |h, _: &ClickEvent, w, cx| {
                    h.run(
                        urfin_core::Command::new("ui.path_edit").arg("side", name),
                        w,
                        cx,
                    );
                })),
        )
        .child(
            div()
                .flex_none()
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(count)),
        )
}

/// Ширина заметки (для обрезки имени).
fn note_width(note: &Note, font: &Font, size: Pixels, window: &mut Window) -> Pixels {
    let measure = |text: &str, sz: Pixels| {
        let run = TextRun {
            len: text.len(),
            font: font.clone(),
            color: Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        window
            .text_system()
            .shape_line(SharedString::from(text.to_owned()), sz, &[run], None)
            .width
    };
    match note {
        Note::Rename { to, .. } => measure("→ ", size) + measure(to, size),
        Note::Tag(t) => measure(t, px(11.0)) + px(12.0),
        Note::Arrow(_) => px(22.0),
    }
}

/// Заметка после имени (D-04): «→ новое» текстом, метка на подложке, стрелка в рамке.
#[allow(clippy::too_many_arguments, reason = "плоский вызов отрисовки")]
#[allow(clippy::many_single_char_names, reason = "координаты строки")]
fn paint_note(
    note: &Note,
    x: Pixels,
    y: Pixels,
    row_h: Pixels,
    font: &Font,
    size: Pixels,
    pal: Pal,
    window: &mut Window,
    cx: &mut App,
) {
    let run = |len: usize, color: Hsla| TextRun {
        len,
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let paint = |window: &mut Window,
                 cx: &mut App,
                 text: SharedString,
                 color: Hsla,
                 sz: Pixels,
                 at: Pixels| {
        let line =
            window
                .text_system()
                .shape_line(text.clone(), sz, &[run(text.len(), color)], None);
        let w = line.width;
        let _ = line.paint(point(at, y), row_h, TextAlign::Left, None, window, cx);
        w
    };
    match note {
        Note::Rename { to, bad } => {
            let w = paint(
                window,
                cx,
                SharedString::new_static("→"),
                pal.secondary,
                size,
                x,
            );
            let color = if *bad { pal.bad } else { pal.accent_text };
            paint(
                window,
                cx,
                SharedString::from(to.clone()),
                color,
                size,
                x + w + px(6.0),
            );
        }
        Note::Tag(t) => {
            let sz = px(11.0);
            let text = SharedString::from(t.clone());
            let w = window
                .text_system()
                .shape_line(text.clone(), sz, &[run(text.len(), pal.secondary)], None)
                .width;
            let h = px(16.0);
            let top = y + (row_h - h) / 2.0;
            window.paint_quad(
                gpui_kit::fill(Bounds::new(point(x, top), size2(w + px(12.0), h)), pal.chip)
                    .corner_radii(px(3.0)),
            );
            paint(window, cx, text, pal.secondary, sz, x + px(6.0));
        }
        Note::Arrow(a) => {
            let h = px(16.0);
            let top = y + (row_h - h) / 2.0;
            window.paint_quad(
                gpui_kit::outline(
                    Bounds::new(point(x, top), size2(px(22.0), h)),
                    pal.accent,
                    gpui_kit::BorderStyle::Solid,
                )
                .corner_radii(px(4.0)),
            );
            paint(
                window,
                cx,
                SharedString::new_static(a),
                pal.accent_text,
                px(11.0),
                x + px(6.0),
            );
        }
    }
}

/// Отступ текста в поле gpui-kit (малое): поле сдвинуто влево, имя не прыгает.
const RENAME_INSET: f32 = 8.0;

/// Текст пути и шапки колонок (D-02), px.
const PATH_TEXT: f32 = 12.5;
const COL_HEAD_TEXT: f32 = 11.5;

/// Ширина сегмента пути с полями и разделитель `\` между сегментами.
const CRUMB_PAD: f32 = 0.0;
const CRUMB_CHEVRON: f32 = 8.0;
/// Место справа от пути под клик «править путь».
const CRUMB_EDIT_MIN: f32 = 32.0;

/// Первый показываемый сегмент: начальные уходят в «…», пока путь не влезет в `avail`.
fn first_shown(
    list: &[(String, std::path::PathBuf)],
    avail: f32,
    measure: &dyn Fn(&str) -> f32,
) -> usize {
    let cost: Vec<f32> = list.iter().map(|(l, _)| measure(l) + CRUMB_PAD).collect();
    let budget = avail - CRUMB_EDIT_MIN;
    #[allow(clippy::cast_precision_loss, reason = "сегментов десятки")]
    let full: f32 = cost.iter().sum::<f32>() + CRUMB_CHEVRON * list.len().saturating_sub(1) as f32;
    if full <= budget || list.len() < 2 {
        return 0;
    }
    let more = measure("…") + CRUMB_PAD + CRUMB_CHEVRON;
    let mut total = full + more;
    let mut first = 0;
    while total > budget && first + 1 < list.len() {
        total -= cost[first] + CRUMB_CHEVRON;
        first += 1;
    }
    first
}

fn col_header(side: Side, theme: &Theme, cols: (f32, f32), cx: &Listen) -> Div {
    let i18n = urfin_i18n::current();
    // Клик по колонке — сортировка по ней (повтор — обратная, S2 §2.1).
    let cell = |key: &str, sort: &'static str, w: Option<f32>| {
        let d = div()
            .id(SharedString::from(format!("col-{sort}-{side:?}")))
            .child(SharedString::from(i18n.get(key)))
            .on_click(cx.listener(move |h, _: &ClickEvent, w, cx| {
                h.run_all(
                    vec![
                        urfin_core::Command::new("panel.focus")
                            .arg("side", super::chrome::side_name(side)),
                        urfin_core::Command::new("panel.sort").arg("key", sort),
                    ],
                    w,
                    cx,
                );
            }));
        match w {
            Some(w) => d.w(px(w)).flex_none().flex().justify_end(),
            None => d.flex_1().min_w_0(),
        }
    };
    div()
        .h(px(theme.m(Metric::HColHeader)))
        .flex_none()
        .flex()
        .items_center()
        .px(px(theme.m(Metric::PadRowX)))
        .text_size(px(COL_HEAD_TEXT))
        .text_color(theme.c(Role::TextSecondary))
        .border_b_1()
        .border_color(theme.c(Role::StrokeDivider))
        .child(cell("ui-col-name", "name", None))
        .when(cols.0 > 0.0, |d| {
            d.child(cell("ui-col-size", "size", Some(cols.0)))
        })
        .when(cols.1 > 0.0, |d| {
            d.child(cell("ui-col-mtime", "mtime", Some(cols.1)))
        })
}

/// Строка видимой страницы для отрисовки: всё готово, `canvas` только рисует.
struct Line {
    /// Имя для `panel.cursor.to`; у `..` — нет.
    target: Option<SharedString>,
    glyph: SharedString,
    name: SharedString,
    size: Option<SharedString>,
    mtime: Option<SharedString>,
    /// Полужирное имя: каталог или `..`.
    strong: bool,
    /// Байты имени на фоне `highlight` (быстрый поиск, отметка по маске).
    hl: Option<(usize, usize)>,
    /// Курсор: `Some(true)` — активной панели (рамка 1 px `accent`, №73), `Some(false)` —
    /// неактивной (не рисуется, как на макетах).
    cursor: Option<bool>,
    /// Отмечена: заливка `cursor`, один оттенок для всех (№73).
    marked: bool,
    color: Hsla,
    /// Иконка: каталог — акцент, скрытый и системный — третичный, прочее — вторичный.
    icon_color: Hsla,
    /// Вместо имени — поле переименования.
    renaming: bool,
    /// Заметка после имени.
    note: Option<Note>,
}

/// Геометрия строки в колонке: отступы и колонки Full/Wide.
#[derive(Clone, Copy)]
struct Geo {
    row_h: f32,
    pad: f32,
    icon_w: f32,
    icon_size: f32,
    size_w: f32,
    mtime_w: f32,
}

/// То же правило колонок, что [`fit_cols`], для колонки списка шириной `w`.
fn fit_cols_geo(geo: &Geo, w: f32) -> (f32, f32) {
    let avail = w - ROW_MX - geo.pad - geo.name_x();
    let (mut size, mut mtime) = (geo.size_w, geo.mtime_w);
    if avail - size - mtime < NAME_MIN {
        mtime = 0.0;
    }
    if avail - size - mtime < NAME_MIN {
        size = 0.0;
    }
    (size, mtime)
}

impl Geo {
    /// Левый край иконки в колонке.
    fn icon_x(&self) -> f32 {
        ROW_MX + self.pad + MARK_W
    }

    /// Левый край имени в колонке.
    fn name_x(&self) -> f32 {
        self.icon_x() + self.icon_w
    }

    /// Ширина имени в колонке шириной `w`.
    fn name_w(&self, w: f32) -> f32 {
        (w - ROW_MX - self.pad - self.size_w - self.mtime_w - self.name_x()).max(0.0)
    }
}

/// Отступ строки от краёв колонки (`mx_1`).
const ROW_MX: f32 = 4.0;
/// Имя не уже этого: в узкой панели сначала уходит дата, потом размер (как в egui).
const NAME_MIN: f32 = 120.0;

/// Ширины колонок размера и даты для панели шириной `w` (0 — колонки нет).
pub(super) fn fit_cols(theme: &Theme, mode: ViewMode, w: f32) -> (f32, f32) {
    let pad = theme.m(Metric::PadRowX) - ROW_MX;
    let name_x = ROW_MX + pad + MARK_W + ICON_PX + ICON_GAP;
    let avail = w - ROW_MX - pad - name_x;
    let mut size = if matches!(mode, ViewMode::Full | ViewMode::Wide) {
        W_SIZE
    } else {
        0.0
    };
    let mut mtime = if matches!(mode, ViewMode::Full) {
        W_MTIME
    } else {
        0.0
    };
    if avail - size - mtime < NAME_MIN {
        mtime = 0.0;
    }
    if avail - size - mtime < NAME_MIN {
        size = 0.0;
    }
    (size, mtime)
}
/// Ширина колонки галочки.
/// Колонки галочки у отмеченных больше нет (№73): отметка — заливка строки.
const MARK_W: f32 = 0.0;
/// Иконка строки (D-03): 14 px, отступ до имени 8.
const ICON_PX: f32 = 14.0;
const ICON_GAP: f32 = 8.0;

/// Цвета строк для `canvas`.
#[derive(Clone, Copy)]
struct Pal {
    secondary: Hsla,
    cursor: Hsla,
    divider: Hsla,
    highlight: Hsla,
    text: Hsla,
    /// Рамка строки переименования и черта под ней (D-04).
    stroke: Hsla,
    accent: Hsla,
    /// Заметки: текст `accent`, проблема `modified`, подложка метки.
    accent_text: Hsla,
    bad: Hsla,
    chip: Hsla,
    icons: bool,
}

/// Видимая страница: Full/Wide — одна колонка, Brief/Medium — колонки сверху вниз.
/// Строки рисует один `canvas` (фон, иконка, текст), без дерева `div`: раскладка taffy
/// для сотни узлов с текстом — половина кадра (G-07, профиль). Клики — по координате.
#[allow(clippy::too_many_arguments, reason = "плоский вызов отрисовки")]
#[allow(clippy::too_many_lines, reason = "сборка строк и отрисовка рядом")]
fn list(
    t: &Tab,
    side: Side,
    active: bool,
    theme: &Theme,
    rows: u32,
    cols: u32,
    field: Option<&super::field::Field>,
    glide: Option<f32>,
    icon_mode: IconMode,
    hl: &Highlight,
    cx: &Listen,
) -> Stateful<Div> {
    let total = t.rows();
    let top = t.cursor.top;
    let rows = rows.max(1);
    let cols = cols.max(1);
    let full = matches!(t.view_mode, ViewMode::Full | ViewMode::Wide);
    let icons = icon_mode;
    let geo = Geo {
        row_h: theme.m(Metric::HRow),
        pad: theme.m(Metric::PadRowX) - ROW_MX,
        icon_w: if icons == IconMode::None {
            0.0
        } else {
            ICON_PX + ICON_GAP
        },
        icon_size: ICON_PX,
        size_w: if full { W_SIZE } else { 0.0 },
        mtime_w: if matches!(t.view_mode, ViewMode::Full) {
            W_MTIME
        } else {
            0.0
        },
    };
    let rename = super::field::rename_input(field, t);
    let dir_links = Some(&hl.links);
    let i18n = urfin_i18n::current();
    let dir_size = SharedString::from(i18n.get("ui-size-dir"));
    let mut lines = Vec::with_capacity((rows * cols) as usize);
    let mut cell = Cell::new();
    for slot in 0..rows * cols {
        let ix = top + slot;
        if ix >= total {
            break;
        }
        let cursor = ix == t.cursor.row;
        let (glyph, name, marked, color, entry) = match t.row(ix) {
            Some(Row::Parent) => (GLYPH_UP, SharedString::new_static(".."), false, None, None),
            Some(Row::Entry { idx, entry }) => {
                let icon = crate::icons::glyph(icon_class(&entry.name, entry.kind, entry.attrs));
                let group = theme.hl.classify(entry);
                let color = theme.hl.color(group, &theme.tokens);
                let name = SharedString::from(entry.name.to_string());
                (icon, name, t.selection.contains(idx), color, Some(entry))
            }
            None => break,
        };
        let dir = entry.is_none_or(|e| e.kind.is_dir_like());
        let mut col = |kind: ColKind| {
            entry.map(|e| {
                cell.clear();
                cell_text(kind, e, &theme.fmt, &mut cell);
                SharedString::from(cell.as_str().to_owned())
            })
        };
        // Ссылка (D-03, `screen-linkview.png`): вид в колонке размера, битая — `modified`.
        let link = entry.filter(|e| {
            matches!(
                e.kind,
                urfin_core::EntryKind::LinkDir
                    | urfin_core::EntryKind::LinkFile
                    | urfin_core::EntryKind::LinkBroken
            )
        });
        let link_info = link.and_then(|e| dir_links.and_then(|l| l.items.get(&e.name)));
        let broken = link.is_some_and(|e| {
            e.kind == urfin_core::EntryKind::LinkBroken || link_info.is_some_and(|l| l.broken)
        });
        let bytes = if !full {
            None
        } else if link.is_some() {
            Some(SharedString::from(i18n.get(match link_info {
                Some(l) if l.junction => "ui-size-junction",
                Some(_) => "ui-size-symlink",
                None => "ui-size-link",
            })))
        } else if dir && entry.is_some() {
            Some(dir_size.clone())
        } else {
            col(ColKind::Size)
        };
        let when = if geo.mtime_w > 0.0 {
            col(ColKind::Mtime)
        } else {
            None
        };
        // Как в Проводнике (№73): текст и раскраска типов у курсора и отмеченных те же.
        // Строка переименования — поле на фоне панели (D-04).
        let renaming = cursor && rename.is_some();
        let kind_color = color.map_or_else(|| theme.c(Role::TextPrimary), super::theme::hsla);
        // Живой предпросмотр строки отметки: совпавшие файлы — как после Enter (№73,
        // отметка везде одинакова).
        let marked = match (&hl.mask, entry) {
            (Some((m, add)), Some(e)) if !dir && m.matches(&e.name) => *add,
            _ => marked,
        };
        // Подсветка `highlight` — только совпавший префикс быстрого поиска.
        let hl_range = entry.and_then(|e| {
            let p = hl.prefix.as_deref().filter(|p| !p.is_empty())?;
            let head = e.name.get(..p.len())?;
            head.eq_ignore_ascii_case(p).then_some((0, p.len()))
        });
        lines.push(Line {
            hl: hl_range,
            target: entry.map(|_| name.clone()),
            glyph: SharedString::from(glyph.to_string()),
            size: bytes,
            mtime: when,
            name,
            strong: dir,
            cursor: cursor.then_some(active),
            marked,
            color: if broken {
                theme.c(Role::Modified)
            } else {
                kind_color
            },
            icon_color: match icons {
                IconMode::Color if dir => theme.c(Role::AccentText),
                IconMode::Color => kind_color,
                _ => theme.c(Role::TextSecondary),
            },
            renaming,
            note: entry.and_then(|e| hl.notes.get(e.name.as_str()).cloned()),
        });
    }
    let lines = Rc::new(lines);
    let bounds = Rc::new(std::cell::Cell::new(Bounds::<Pixels>::default()));
    let pal = Pal {
        secondary: theme.c(Role::TextSecondary),
        cursor: theme.c(Role::FillCursor),
        divider: theme.c(Role::StrokeDivider),
        highlight: theme.c(Role::Highlight),
        text: theme.c(Role::TextPrimary),
        stroke: theme.c(Role::StrokeControl),
        accent: theme.c(Role::AccentFill),
        accent_text: theme.c(Role::AccentText),
        bad: theme.c(Role::Modified),
        chip: theme.c(Role::FillControl),
        icons: icons != IconMode::None,
    };
    let paint_lines = lines.clone();
    let paint_bounds = bounds.clone();
    let canvas = canvas(
        move |b, _, _| paint_bounds.set(b),
        move |b, (), window, cx| {
            paint(&paint_lines, b, rows, cols, geo, pal, glide, window, cx);
        },
    )
    .size_full();
    // Строка под точкой → цель для `panel.cursor.to` (`None` снаружи — мимо строк).
    let hit = move |lines: &[Line], p: Point<Pixels>| -> Option<Option<SharedString>> {
        let b = bounds.get();
        if !b.contains(&p) {
            return None;
        }
        #[allow(clippy::cast_precision_loss, reason = "колонок единицы")]
        let col_w = f32::from(b.size.width) / cols as f32;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "не меньше 0"
        )]
        let c = (f32::from(p.x - b.origin.x) / col_w).floor().max(0.0) as u32;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "не меньше 0"
        )]
        let r = (f32::from(p.y - b.origin.y) / geo.row_h).floor().max(0.0) as u32;
        if r >= rows {
            return None;
        }
        lines
            .get((c.min(cols - 1) * rows + r) as usize)
            .map(|l| l.target.clone())
    };
    let click_lines = lines.clone();
    let click_hit = hit.clone();
    let move_lines = lines.clone();
    let move_hit = hit.clone();
    let right_lines = lines.clone();
    let down_lines = lines.clone();
    let down_hit = hit.clone();
    let mut d = div()
        .id(SharedString::from(format!("list-{side:?}")))
        .flex_1()
        .min_h_0()
        .relative()
        .overflow_hidden()
        .child(canvas)
        .on_scroll_wheel(cx.listener(move |h, ev: &ScrollWheelEvent, w, cx| {
            h.wheel(side, ev, w, cx);
        }))
        // Shift+ЛКМ — пометка на нажатии и протяжка (S0 №74).
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |h, ev: &MouseDownEvent, w, cx| {
                if ev.modifiers.shift
                    && let Some(target) = down_hit(&down_lines, ev.position)
                {
                    h.row_mark_down(side, target.as_deref(), w, cx);
                }
            }),
        )
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |h, ev: &MouseUpEvent, w, cx| {
                if ev.modifiers.shift || h.marking() {
                    h.right_drag_end();
                } else if let Some(target) = click_hit(&click_lines, ev.position) {
                    h.row_click(
                        side,
                        target.as_deref(),
                        ev.click_count,
                        ev.modifiers.control,
                        w,
                        cx,
                    );
                }
            }),
        )
        // ПКМ — контекстное меню у мыши (S0 №74).
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |h, ev: &MouseDownEvent, w, cx| {
                if let Some(target) = hit(&right_lines, ev.position) {
                    let at = (f32::from(ev.position.x), f32::from(ev.position.y));
                    h.row_context(side, target.as_deref(), at, w, cx);
                }
            }),
        )
        // Протяжка после Shift+ЛКМ помечает строки под мышью.
        .on_mouse_move(cx.listener(move |h, ev: &MouseMoveEvent, w, cx| {
            if ev.pressed_button != Some(MouseButton::Left) {
                h.right_drag_end();
            } else if let Some(target) = move_hit(&move_lines, ev.position) {
                h.row_right_drag(side, target.as_deref(), w, cx);
            }
        }));
    // Переименование на месте (S2-16): поле поверх имени в строке курсора.
    if let Some(input) = rename
        && let Some(k) = lines.iter().position(|l| l.renaming)
    {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_precision_loss,
            reason = "номер строки на странице"
        )]
        let (top, col, n_cols) = (
            (k as u32 % rows) as f32 * geo.row_h,
            (k as u32 / rows) as f32,
            cols as f32,
        );
        d = d.child(
            div()
                .absolute()
                .top(px(top))
                .h(px(geo.row_h))
                .left(gpui_kit::relative(col / n_cols))
                .w(gpui_kit::relative(1.0 / n_cols))
                .pl(px(geo.name_x() - RENAME_INSET))
                .pr(px(ROW_MX + geo.pad + geo.size_w + geo.mtime_w))
                .flex()
                .items_center()
                .child(div().flex_1().min_w_0().child(input)),
        );
    }
    d
}

/// Рисует строки страницы как egui-панель (S0-decisions №31–32): фон помеченных и галочка,
/// курсор — заливка, у активной панели ещё рамка 1 px; иконка, имя (с «…»), колонки.
#[allow(clippy::too_many_arguments, reason = "плоский вызов отрисовки")]
#[allow(clippy::too_many_lines, reason = "строка слева направо")]
fn paint(
    lines: &[Line],
    b: Bounds<Pixels>,
    rows: u32,
    cols: u32,
    geo: Geo,
    pal: Pal,
    glide: Option<f32>,
    window: &mut Window,
    cx: &mut App,
) {
    let style = window.text_style();
    let size = style.font_size.to_pixels(window.rem_size());
    let font = style.font();
    let strong = Font {
        weight: gpui_kit::FontWeight::MEDIUM,
        ..font.clone()
    };
    let icons = gpui_kit::font(ICON_FONT);
    let row_h = px(geo.row_h);
    #[allow(clippy::cast_precision_loss, reason = "колонок единицы")]
    let col_w = f32::from(b.size.width) / cols as f32;
    let (size_w, mtime_w) = fit_cols_geo(&geo, col_w);
    let geo = Geo {
        size_w,
        mtime_w,
        ..geo
    };
    let name_w = px(geo.name_w(col_w));
    let run = |len: usize, font: &Font, color: Hsla| TextRun {
        len,
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let shape = |window: &mut Window, s: &SharedString, font: &Font, color: Hsla, sz: Pixels| {
        window
            .text_system()
            .shape_line(s.clone(), sz, &[run(s.len(), font, color)], None)
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "номер строки на странице"
    )]
    let at = |k: usize| {
        (
            b.origin.x + px((k as u32 / rows) as f32 * col_w),
            b.origin.y + row_h * ((k as u32 % rows) as f32),
        )
    };
    window.with_content_mask(Some(ContentMask { bounds: b }), |window| {
        // Сначала фоны, потом содержимое. Как в Проводнике (№73): отмеченные — заливка
        // `cursor`, курсор активной панели — рамка 1 px `accent` (на отмеченной — поверх
        // заливки), у неактивной панели курсора не видно.
        for (k, l) in lines.iter().enumerate() {
            let (x0, y) = at(k);
            if l.marked {
                let row = Bounds::new(point(x0, y), size2(px(col_w), row_h));
                window.paint_quad(gpui_kit::fill(row, pal.cursor));
            }
        }
        for (k, l) in lines.iter().enumerate() {
            let (x0, y) = at(k);
            if let Some(active) = l.cursor {
                // Эксперимент `slide`: полоса между прежней и новой строкой.
                let y = match glide {
                    Some(f) => b.origin.y + row_h * f,
                    None => y,
                };
                let row = Bounds::new(point(x0, y), size2(px(col_w), row_h));
                if l.renaming {
                    // Вся строка — поле: рамка `border`, снизу черта 2 `accent`.
                    window.paint_quad(gpui_kit::outline(
                        row,
                        pal.stroke,
                        gpui_kit::BorderStyle::Solid,
                    ));
                    window.paint_quad(gpui_kit::fill(
                        Bounds::new(point(x0, y + row_h - px(2.0)), size2(px(col_w), px(2.0))),
                        pal.accent,
                    ));
                    continue;
                }
                if active {
                    window.paint_quad(gpui_kit::outline(
                        row,
                        pal.accent,
                        gpui_kit::BorderStyle::Solid,
                    ));
                }
            }
        }
        // Колонки Brief/Medium разделены линиями `border` (D-03).
        for c in 1..cols {
            #[allow(clippy::cast_precision_loss, reason = "колонок единицы")]
            let x = b.origin.x + px(c as f32 * col_w);
            window.paint_quad(gpui_kit::fill(
                Bounds::new(point(x, b.origin.y), size2(px(1.0), b.size.height)),
                pal.divider,
            ));
        }
        for (k, l) in lines.iter().enumerate() {
            let (x0, y) = at(k);
            let secondary = pal.secondary;
            if pal.icons {
                let icon = shape(window, &l.glyph, &icons, l.icon_color, px(geo.icon_size));
                let _ = icon.paint(
                    point(x0 + px(geo.icon_x()), y),
                    row_h,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
            if !l.renaming {
                let f = if l.strong { &strong } else { &font };
                // Имя одним или тремя кусками: до подсветки, подсвеченное, после.
                let runs: Vec<TextRun> = match l.hl {
                    Some((from, to)) if to <= l.name.len() && from < to => {
                        let mut mid = run(to - from, f, pal.text);
                        mid.background_color = Some(pal.highlight);
                        [
                            (from > 0).then(|| run(from, f, l.color)),
                            Some(mid),
                            (to < l.name.len()).then(|| run(l.name.len() - to, f, l.color)),
                        ]
                        .into_iter()
                        .flatten()
                        .collect()
                    }
                    _ => vec![run(l.name.len(), f, l.color)],
                };
                let mut line = window
                    .text_system()
                    .shape_line(l.name.clone(), size, &runs, None);
                // Заметке — место после имени: имя обрезается раньше (но не короче половины).
                let room = l.note.as_ref().map_or(name_w, |n| {
                    (name_w - note_width(n, &font, size, window) - px(8.0)).max(name_w / 2.0)
                });
                if line.width > room {
                    let (cut, runs) = cx
                        .text_system()
                        .line_wrapper(f.clone(), size)
                        .truncate_line(l.name.clone(), room, "…", &runs, TruncateFrom::End);
                    line = window.text_system().shape_line(cut, size, &runs, None);
                }
                let _ = line.paint(
                    point(x0 + px(geo.name_x()), y),
                    row_h,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
                if let Some(note) = &l.note {
                    let end = x0 + px(geo.name_x()) + line.width.min(name_w) + px(8.0);
                    paint_note(note, end, y, row_h, &font, size, pal, window, cx);
                }
            }
            // Колонки прижаты вправо: дата у края, размер левее.
            let mut right = x0 + px(col_w - ROW_MX - geo.pad);
            for (s, w) in [(&l.mtime, geo.mtime_w), (&l.size, geo.size_w)] {
                if w <= 0.0 {
                    continue;
                }
                if let Some(s) = s {
                    let line = shape(window, s, &font, secondary, size);
                    let _ = line.paint(
                        point(right - line.width, y),
                        row_h,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
                right -= px(w);
            }
        }
    });
}
