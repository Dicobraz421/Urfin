//! Панель файлов на GPUI (G-04, S2 §2.1–2.3): карточка с путём, шапкой колонок,
//! строками 24 px и подвалом. Рисуется только видимая страница от `cursor.top` —
//! её держит Core по `panel.viewport` (S0 v3 №3).

use gpui_kit::prelude::*;
use smallvec::SmallVec;
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
use crate::columns::{self, Column, ColumnSet, Slot};
use crate::config::IconMode;
use crate::fmt_cell::{Cell, ColKind, cell_text};
use crate::tokens::{Metric, Role};

/// Минимальная ширина колонки Brief.
/// Brief — только имена в три колонки (D-03).
const BRIEF_COLS: f32 = 3.0;
/// Шрифт иконок ОС (S2 §1.6).
const ICON_FONT: &str = "Segoe Fluent Icons";
/// `..` — стрелка вверх.
const GLYPH_UP: char = '\u{E74A}';
/// Строка вкладки пакетов (`Package`, `screen-pkginstalled`).
const GLYPH_PACKAGE: char = '\u{E7B8}';

/// Геометрия панели для `panel.viewport`: строк в колонке и колонок.
pub(super) fn grid(theme: &Theme, mode: ViewMode, _w: f32, h: f32) -> (u32, u32) {
    let head = if columns::has_header(mode) {
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
    #[allow(clippy::too_many_lines, reason = "три части панели в одном match")]
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
            // Карта анализа места во второй панели (F01-07): вместо пути и списка.
            Piece::Head if super::du_map::shows(s, t) => {
                body.child(super::du_map::head(s, side, theme))
            }
            Piece::List if super::du_map::shows(s, t) => {
                body.child(super::du_map::body(s, side, theme, h.hover_paint(side), &l))
            }
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
                let sets = view_modes(s);
                let cols = fit_cols(theme, &columns::for_tab(s, t, sets), panel_w);
                let du = crate::du_view::info(s, t);
                let pkg = crate::pkg_view::info(s, t);
                let bar = match (&du, &pkg) {
                    // Вкладка анализа места (F01-05): чип «Анализ», путь, итоги, чипы фильтров.
                    (Some(info), _) => {
                        super::du::path_bar(s, side, info, theme, h.field.as_ref(), &l)
                    }
                    // Вкладка пакетов (F02-08): чип «Пакеты», менеджеры, число программ.
                    (None, Some(info)) => {
                        super::pkg::path_bar(s, side, t, info, theme, h.field.as_ref(), &l)
                    }
                    (None, None) => path_bar(
                        s,
                        t,
                        side,
                        s.active == side,
                        theme,
                        h.field.as_ref(),
                        panel_w,
                        columns::custom(t.view_mode, sets),
                        &measure,
                        &l,
                    ),
                };
                // Вкладка пакетов: первая колонка — «Программа».
                let name_key = pkg.as_ref().map(|_| {
                    if crate::pkg_view::filter(t) == Some(urfin_pkg::Filter::Search) {
                        "pkg-col-name-search"
                    } else {
                        "pkg-col-name"
                    }
                });
                body.child(bar)
                    .children(du.as_ref().map(|_| super::du::chips(t, side, theme, &l)))
                    .children(du.as_ref().and_then(|_| super::du::temp_warning(t, theme)))
                    .children(
                        pkg.as_ref()
                            .map(|i| super::pkg::chips(s, side, t, i, theme, &l)),
                    )
                    .when(
                        columns::has_header(columns::mode_of(s, t)) && state_of(t).is_none(),
                        |d| {
                            d.child(col_header(
                                side,
                                theme,
                                &cols,
                                name_key.unwrap_or_else(|| crate::du_view::name_header(t)),
                                &l,
                            ))
                        },
                    )
            }
            // Дерево, «Сведения», просмотр (S2 §2.4–2.5).
            Piece::List if t.mode != urfin_core::PanelMode::Files => {
                body.child(super::views::body(s, side, theme, rows))
            }
            Piece::List if state_of(t).is_some() => body.child(state_view(t, side, theme, &l)),
            Piece::List => body
                .child(list(
                    t,
                    side,
                    s.active == side,
                    theme,
                    rows,
                    cols,
                    &columns::for_tab(s, t, view_modes(s)),
                    s.ext
                        .get::<urfin_vfs::Schemes>()
                        .and_then(|r| r.with_columns(&t.loc))
                        .map(AsRef::as_ref),
                    h.field.as_ref(),
                    h.slide_pos(side),
                    h.hover_paint(side),
                    s.ext
                        .get::<crate::UiState>()
                        .map_or(IconMode::default(), |u| u.icons),
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
                        du: crate::du_view::look(s, t),
                        du_root: crate::du_view::info(s, t)
                            .filter(|_| {
                                crate::du_view::filter(t) == Some(urfin_vfs::du::DuFilter::Dups)
                            })
                            .map(|i| i.root.display().to_string()),
                        pkg: crate::pkg_view::book(s, t).map(|b| b.view()),
                    },
                    &l,
                ))
                // Вкладка анализа места: «Отмечено N · освободится X» (F01-06).
                .children(super::du::mark_bar(s, side, t, theme, &l))
                // Вкладка пакетов: «Обновить N» при отметке в «Обновлениях» (F02-08).
                .children(
                    crate::pkg_view::info(s, t)
                        .and_then(|i| super::pkg::upgrade_bar(s, side, t, &i, theme, &l)),
                ),
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
    /// Вкладка анализа места: вид (фильтр) и раскрытые группы «Типов» (F01-08).
    pub du: Option<(urfin_vfs::du::DuFilter, u16)>,
    /// Корень анализа — у «Дубликатов» копия показывает, где лежит (F01-09).
    pub du_root: Option<String>,
    /// Вкладка пакетов (F02-08): строки модели — тон ячеек и метка «важное».
    pub pkg: Option<std::sync::Arc<urfin_pkg::View>>,
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
    /// Подсказка «что это» у строки «Временных» — вторичным цветом; `warn` — цветом
    /// предупреждения («нет доступа», `screen-dutemp`). `label` — понятное имя строки
    /// («%TEMP%»), путь — серым после него; `badge` — что будет после удаления.
    Hint {
        text: String,
        warn: bool,
        label: Option<String>,
        badge: Option<(String, Tone)>,
    },
    /// Заголовок группы «Дубликатов» (`screen-dudups`): «2 копии» вторичным цветом и
    /// метка в рамке — «✓ совпадает по хэшу» (`accent`) или «только размер».
    Dup {
        copies: String,
        badge: String,
        ok: bool,
    },
    /// Метка в рамке `accent`: «важное» в «Обновлениях» пакетов (F02-08).
    Badge(String),
}

/// Наборы колонок `[ui.view_modes]` (S2-19).
fn view_modes(s: &AppState) -> &[ColumnSet] {
    s.ext
        .get::<crate::UiState>()
        .map_or(&[][..], |u| &u.view_modes)
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
    // Карта анализа места на месте этой панели: её вкладка, курсор, отметка (F01-07).
    super::du_map::print(s, side).hash(&mut h);
    let icons = s.ext.get::<crate::UiState>().map(|u| u.icons as u8);
    icons.hash(&mut h);
    format!("{:?}{:?}", t.view_mode, t.mode).hash(&mut h);
    // Колонки вида и подпись набора (перечитан конфиг — набор мог измениться).
    let sets = view_modes(s);
    for c in columns::for_tab(s, t, sets).iter() {
        (c.kind, c.w.to_bits()).hash(&mut h);
    }
    columns::custom(t.view_mode, sets)
        .map(|set| set.title.as_str())
        .hash(&mut h);
    if let Some(f) = field {
        format!("{:?}{}", f.key, f.input.entity_id()).hash(&mut h);
    }
    match piece {
        Piece::Head => {
            // Тон заголовка активной панели (палитра B).
            active.hash(&mut h);
            format!("{:?}", t.loc).hash(&mut h);
            // Ctrl+D на узкой панели показывает скрытую кнопку «Закладки» (S7-25a).
            (
                active,
                s.ext
                    .get::<crate::UiState>()
                    .and_then(crate::menu::open_id)
                    .is_some_and(|id| id == "bookmarks"),
            )
                .hash(&mut h);
            t.filter.as_ref().map(urfin_core::Mask::as_str).hash(&mut h);
            (t.listing.order.len(), t.listing.entries.len()).hash(&mut h);
            // Пинг панели `sftp://`.
            crate::ssh::ping(s, t).hash(&mut h);
            // Вкладка анализа места: итоги и подсчёт меняются тиками скана.
            if crate::du_view::is_du(t) {
                crate::du_view::rev(s).hash(&mut h);
                format!("{:?}", crate::du_view::info(s, t)).hash(&mut h);
            }
            // Вкладка пакетов: ход менеджеров, числа, фильтр; бегущий отрезок проверки.
            if let Some(i) = crate::pkg_view::info(s, t) {
                (crate::pkg_view::rev(s), &i, anim).hash(&mut h);
            }
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
            // Тон ячеек и «важное» вкладки пакетов — из модели.
            crate::pkg_view::is_pkg(t)
                .then(|| crate::pkg_view::rev(s))
                .hash(&mut h);
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

/// Высота шапки таба: у вкладки анализа места — ещё строка чипов фильтров.
pub(super) fn head_h_tab(s: &AppState, theme: &Theme, t: &Tab, state: bool) -> f32 {
    // Карта анализа места: только строка «Карта · путь».
    if super::du_map::shows(s, t) {
        return theme.m(Metric::HBreadcrumb);
    }
    head_h(theme, columns::mode_of(s, t), state)
        + super::du::head_extra(t)
        + if crate::pkg_view::is_pkg(t) {
            super::pkg::CHIPS_H
        } else {
            0.0
        }
}

/// Высота шапки панели: путь и, в Full и наборах колонок, строка колонок (у состояния
/// папки — без неё).
pub(super) fn head_h(theme: &Theme, mode: ViewMode, state: bool) -> f32 {
    theme.m(Metric::HBreadcrumb)
        + if columns::has_header(mode) && !state {
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

/// Путь панели (D-02, S2-12): слева кнопки «История» и «Закладки» (S7-25a), за ними
/// строка пути `C:\Dev\urfin` — у активной панели цвет `text` и подчёркивание `accent`
/// 2 px, у другой — `secondary`; справа «N объектов». Сегмент — переход к нему, пустое
/// место — правка пути. Узкая панель (`screen-panelhead.png`): сначала уходит
/// «N объектов», затем средние сегменты сворачиваются в «…» (диск и текущая папка
/// видны), затем скрывается «Закладки» (Ctrl+D показывает её открытой). В наборе колонок
/// справа — переключатель набора `«…» ▾` (S2 §2.3).
#[allow(clippy::too_many_arguments, reason = "плоский вызов отрисовки")]
#[allow(clippy::too_many_lines, reason = "сегменты слева направо")]
fn path_bar(
    s: &AppState,
    t: &Tab,
    side: Side,
    active: bool,
    theme: &Theme,
    field: Option<&super::field::Field>,
    width: f32,
    set: Option<&ColumnSet>,
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
        .border_color(theme.c(Role::StrokeDivider))
        // Палитра B: заголовок активной панели — тон `accent_soft`.
        .when(active && theme.vivid(), |d| d.bg(theme.c(Role::AccentSoft)));
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
    // Панель `sftp://` (`screen-sftppanel`): чип «SSH» у пути, справа — «N объектов · ping».
    let ssh = crate::ssh::profile_of(t).is_some();
    let count = match crate::ssh::ping(s, t) {
        Some(ms) if !count.is_empty() => {
            let mut a = FluentArgs::new();
            a.set("count", count);
            a.set("ms", ms);
            i18n.fmt("net-head-ping", &a)
        }
        _ => count,
    };
    // Узкая панель: что помещается (порядок — DESIGN «Метрики окна»).
    let bm_open = s.active == side
        && s.ext
            .get::<crate::UiState>()
            .and_then(crate::menu::open_id)
            .is_some_and(|id| id == "bookmarks");
    let path_w = if t.loc.is_local() {
        crumbs_w(&list, measure)
    } else {
        measure(&t.loc.to_uri())
    };
    let count_w = if count.is_empty() {
        0.0
    } else {
        measure(&count) + 8.0
    };
    let set_w = set.map_or(0.0, |set| measure(set.title.as_str()) + 34.0)
        + if ssh { SSH_CHIP_W } else { 0.0 };
    let fit = fit_head(
        width - 2.0 * theme.m(Metric::PadRowX) - set_w - CRUMB_EDIT_MIN,
        path_w,
        count_w,
        min_path_w(&list, measure),
    );
    let fit = Fit {
        bookmarks: fit.bookmarks || bm_open,
        ..fit
    };
    let avail = width
        - 2.0 * theme.m(Metric::PadRowX)
        - set_w
        - head_buttons_w(fit.bookmarks)
        - if fit.count { count_w } else { 0.0 };
    let first = first_shown(&list, avail, measure);
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
        // Диск остаётся, «…» — к последнему скрытому сегменту.
        let (drive, drive_path) = &list[0];
        let (_, path) = &list[first - 1];
        segs = segs
            .child(seg(
                format!("crumb-{name}-0"),
                SharedString::from(drive.clone()),
                drive_path.clone(),
            ))
            .child(sep())
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
    let switcher = set.map(|set| {
        div()
            .id(SharedString::from(format!("columns-{name}")))
            .flex_none()
            .ml(px(8.0))
            .h(px(22.0))
            .px(px(6.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .rounded(px(4.0))
            .hover(|d| d.bg(theme.c(Role::FillSubtleHover)))
            .text_color(theme.c(Role::TextPrimary))
            .child(SharedString::from(set.title.to_string()))
            .child(
                div()
                    .text_size(px(9.0))
                    .text_color(theme.c(Role::TextSecondary))
                    .child(SharedString::new_static("▾")),
            )
            .on_click(cx.listener(move |h, _: &ClickEvent, w, cx| {
                h.run_all(
                    vec![
                        urfin_core::Command::new("panel.focus").arg("side", name),
                        urfin_core::Command::new("ui.menu").arg("menu", format!("columns-{name}")),
                    ],
                    w,
                    cx,
                );
            }))
    });
    bar.child(head_buttons(s, side, fit.bookmarks, theme, cx))
        .children(ssh.then(|| ssh_chip(theme)))
        .child(segs)
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
        .children(fit.count.then(|| {
            div()
                .flex_none()
                .text_color(theme.c(Role::TextSecondary))
                .child(SharedString::from(count))
        }))
        .children(switcher)
}

/// Ширина чипа «SSH» с зазором до пути.
const SSH_CHIP_W: f32 = 62.0;

/// Чип «SSH» перед путём `sftp://` (`screen-sftppanel`): значок терминала и подпись в
/// рамке `border`, как чип кодировки.
fn ssh_chip(theme: &Theme) -> Div {
    div()
        .flex_none()
        .mr(px(8.0))
        .h(px(22.0))
        .px(px(6.0))
        .flex()
        .items_center()
        .gap(px(5.0))
        .rounded(px(5.0))
        .border_1()
        .border_color(theme.c(Role::StrokeDivider))
        .bg(theme.c(Role::LayerPanel))
        .text_size(px(12.0))
        .text_color(theme.c(Role::TextPrimary))
        .child(
            div()
                .font_family(ICON_FONT)
                .text_size(px(12.0))
                .child(SharedString::new_static("\u{E756}")),
        )
        .child(SharedString::new_static("SSH"))
}

/// Кнопки заголовка панели (DESIGN «Метрики окна», `screen-panelhead.png`): 24×24, шаг 2,
/// до пути 8; значок Segoe Fluent Icons 14 `secondary`.
pub(super) const HEAD_BTN: f32 = 24.0;
pub(super) const HEAD_BTN_STEP: f32 = 2.0;
const HEAD_BTN_GAP: f32 = 8.0;
/// `History`, `FavoriteList`.
pub(super) const GLYPH_HISTORY: &str = "\u{E81C}";
pub(super) const GLYPH_BOOKMARKS: &str = "\u{E728}";

/// Ширина кнопок слева от пути (с зазором до пути).
fn head_buttons_w(bookmarks: bool) -> f32 {
    if bookmarks {
        2.0 * HEAD_BTN + HEAD_BTN_STEP + HEAD_BTN_GAP
    } else {
        HEAD_BTN + HEAD_BTN_GAP
    }
}

/// Что помещается в заголовке панели.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fit {
    count: bool,
    bookmarks: bool,
}

/// Узкая панель (решение 2026-11-01 «Панели»): место `avail` под кнопки, путь и счётчик;
/// сначала уходит «N объектов», затем путь сокращается до `min_path` (диск, «…»,
/// последняя папка), затем — кнопка «Закладки». «История» остаётся всегда.
fn fit_head(avail: f32, path: f32, count: f32, min_path: f32) -> Fit {
    let both = head_buttons_w(true);
    if both + path + count <= avail {
        return Fit {
            count: true,
            bookmarks: true,
        };
    }
    Fit {
        count: false,
        bookmarks: both + min_path.min(path) <= avail,
    }
}

/// Ширина пути целиком.
fn crumbs_w(list: &[(String, std::path::PathBuf)], measure: &dyn Fn(&str) -> f32) -> f32 {
    #[allow(clippy::cast_precision_loss, reason = "сегментов десятки")]
    let chev = CRUMB_CHEVRON * list.len().saturating_sub(1) as f32;
    list.iter()
        .map(|(l, _)| measure(l) + CRUMB_PAD)
        .sum::<f32>()
        + chev
}

/// Ширина самого короткого пути: диск, «…», последняя папка.
fn min_path_w(list: &[(String, std::path::PathBuf)], measure: &dyn Fn(&str) -> f32) -> f32 {
    match list {
        [] => 0.0,
        [only] => measure(&only.0),
        [drive, .., folder] => {
            measure(&drive.0) + measure("…") + measure(&folder.0) + 2.0 * CRUMB_CHEVRON
        }
    }
}

/// Кнопки «История» (Alt+F12 / Alt+F11) и «Закладки» (Ctrl+D) панели `side`: клик
/// делает панель активной и открывает «мешочек» от кнопки; подсказка — названия команд
/// и клавиши из keymap.
pub(super) fn head_buttons(
    s: &AppState,
    side: Side,
    bookmarks: bool,
    theme: &Theme,
    cx: &Listen,
) -> Div {
    let i18n = urfin_i18n::current();
    let name = super::chrome::side_name(side);
    let button = |id: String, glyph: &'static str, cmd: urfin_core::Command| {
        div()
            .id(SharedString::from(id))
            .flex_none()
            .size(px(HEAD_BTN))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.0))
            .font_family(ICON_FONT)
            .text_size(px(14.0))
            .text_color(theme.c(Role::TextSecondary))
            .hover(|d| {
                d.bg(theme.c(Role::CurrentLine))
                    .text_color(theme.c(Role::TextPrimary))
            })
            .active(|d| d.bg(theme.c(Role::StrokeControl)))
            .child(SharedString::new_static(glyph))
            .on_click(cx.listener(move |h, _: &ClickEvent, w, cx| {
                h.run_all(
                    vec![
                        urfin_core::Command::new("panel.focus").arg("side", name),
                        cmd.clone(),
                    ],
                    w,
                    cx,
                );
            }))
    };
    let dirs = urfin_core::Command::new("ui.history.dirs");
    let files = urfin_core::Command::new("viewer.history");
    let marks = urfin_core::Command::new("ui.menu").arg("menu", "bookmarks");
    let history = super::view::tip_lines(
        button(format!("head-history-{name}"), GLYPH_HISTORY, dirs.clone()),
        theme,
        vec![
            (
                i18n.get("cmd-ui-history-dirs"),
                super::view::key_for(s, &dirs),
            ),
            (
                i18n.get("cmd-viewer-history"),
                super::view::key_for(s, &files),
            ),
        ],
    );
    let mut row = div()
        .flex_none()
        .mr(px(HEAD_BTN_GAP))
        .flex()
        .items_center()
        .gap(px(HEAD_BTN_STEP))
        .child(history);
    if bookmarks {
        row = row.child(super::view::tip_lines(
            button(
                format!("head-bookmarks-{name}"),
                GLYPH_BOOKMARKS,
                marks.clone(),
            ),
            theme,
            vec![(i18n.get("ui-bookmarks"), super::view::key_for(s, &marks))],
        ));
    }
    row
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
        Note::Tag(t) | Note::Badge(t) => measure(t, px(11.0)) + px(12.0),
        Note::Arrow(_) => px(22.0),
        // Метка последствий — только если влезает (её место имени не отнимает).
        Note::Hint { text, .. } => measure(text, size),
        Note::Dup { copies, badge, .. } => {
            measure(copies, size) + px(10.0) + measure(badge, px(11.0)) + px(12.0)
        }
    }
}

/// Заметка после имени (D-04): «→ новое» текстом, метка на подложке, стрелка в рамке.
#[allow(clippy::too_many_arguments, reason = "плоский вызов отрисовки")]
#[allow(clippy::too_many_lines, reason = "по ветке на вид заметки")]
#[allow(clippy::many_single_char_names, reason = "координаты строки")]
fn paint_note(
    note: &Note,
    x: Pixels,
    limit: Pixels,
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
        Note::Hint {
            text, warn, badge, ..
        } => {
            let color = if *warn { pal.warn } else { pal.secondary };
            let w = paint(window, cx, text.clone().into(), color, size, x);
            if let Some((b, tone)) = badge {
                let gap = if text.is_empty() { px(0.0) } else { px(10.0) };
                let bw = window
                    .text_system()
                    .shape_line(b.clone().into(), px(11.0), &[run(b.len(), color)], None)
                    .width
                    + px(12.0);
                if x + w + gap + bw <= limit {
                    paint_badge(b, *tone, x + w + gap, y, row_h, font, pal, window, cx);
                }
            }
        }
        Note::Dup { copies, badge, ok } => {
            let w = paint(
                window,
                cx,
                SharedString::from(copies.clone()),
                pal.secondary,
                size,
                x,
            );
            paint_badge(
                badge,
                if *ok { Tone::Ok } else { Tone::Plain },
                x + w + px(10.0),
                y,
                row_h,
                font,
                pal,
                window,
                cx,
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
        Note::Badge(t) => paint_badge(t, Tone::Ok, x, y, row_h, font, pal, window, cx),
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

/// Тон метки в рамке: `accent` (совпало по хэшу, безопасно), обычный, предупреждение.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Tone {
    Ok,
    Plain,
    Warn,
}

/// Метка в рамке: у заголовка группы «Дубликатов» (совпало по хэшу — `accent`), у
/// строки «Временных» — что будет после удаления.
#[allow(clippy::too_many_arguments, reason = "плоский вызов отрисовки")]
fn paint_badge(
    badge: &str,
    tone: Tone,
    x: Pixels,
    y: Pixels,
    row_h: Pixels,
    font: &Font,
    pal: Pal,
    window: &mut Window,
    cx: &mut App,
) {
    let sz = px(11.0);
    let text = SharedString::from(badge.to_owned());
    let (color, border) = match tone {
        Tone::Ok => (pal.accent_text, pal.accent),
        Tone::Plain => (pal.secondary, pal.stroke),
        Tone::Warn => (pal.warn, pal.warn),
    };
    let runs = [TextRun {
        len: text.len(),
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }];
    let line = window.text_system().shape_line(text, sz, &runs, None);
    let h = px(18.0);
    let top = y + (row_h - h) / 2.0;
    window.paint_quad(
        gpui_kit::outline(
            Bounds::new(point(x, top), size2(line.width + px(12.0), h)),
            border,
            gpui_kit::BorderStyle::Solid,
        )
        .corner_radii(px(4.0)),
    );
    let _ = line.paint(
        point(x + px(6.0), y),
        row_h,
        TextAlign::Left,
        None,
        window,
        cx,
    );
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
    // Диск остаётся первым (`C:\…\src`): сворачиваются сегменты за ним.
    let more = measure("…") + CRUMB_PAD + CRUMB_CHEVRON;
    let mut total = full + more;
    let mut first = 1;
    while total > budget && first + 1 < list.len() {
        total -= cost[first] + CRUMB_CHEVRON;
        first += 1;
    }
    if first == 1 { 0 } else { first }
}

/// Строка заголовков: имя и колонки вида на тех же местах, что в строках списка.
fn col_header(
    side: Side,
    theme: &Theme,
    cols: &[Slot],
    name_key: &'static str,
    cx: &Listen,
) -> Div {
    let i18n = urfin_i18n::current();
    // Клик по колонке — сортировка по ней (повтор — обратная, S2 §2.1); у колонок без
    // ключа сортировки клика нет. Подпись имени — своя у видов анализа места.
    let cell = |kind: ColKind, w: Option<f32>| {
        let key = if kind == ColKind::Name {
            Some(name_key)
        } else {
            kind.header_key()
        };
        let label = key.map(|k| i18n.get(k)).unwrap_or_default();
        let mut d = div()
            .id(SharedString::from(format!("col-{}-{side:?}", kind.id())))
            .child(SharedString::from(label));
        if let Some(sort) = kind.sort_key() {
            d = d.on_click(cx.listener(move |h, _: &ClickEvent, w, cx| {
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
        }
        match w {
            Some(w) if kind.right_aligned() => d.w(px(w)).flex_none().flex().justify_end(),
            Some(w) => d
                .w(px(w))
                .flex_none()
                .pl(px(columns::GAP))
                .overflow_hidden()
                .whitespace_nowrap(),
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
        .child(cell(ColKind::Name, None))
        .children(cols.iter().map(|c| cell(c.kind, Some(c.w))))
}

/// Строка видимой страницы для отрисовки: всё готово, `canvas` только рисует.
struct Line {
    /// Имя для `panel.cursor.to`; у `..` — нет.
    target: Option<SharedString>,
    glyph: SharedString,
    name: SharedString,
    /// Тексты колонок вида по порядку (Full — размер и дата).
    cells: SmallVec<[Option<SharedString>; 4]>,
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
    /// Вкладка анализа места: доля в папке (0…1) и «39 %»; размер недосчитан («≥ X»).
    du: Option<DuCell>,
    /// Байты начала имени вторичным цветом (папка `dir\` в видах фильтров).
    dim: usize,
    /// Байты имени с этого места — вторичным цветом (путь после понятного имени).
    tail: usize,
    /// Отступ строки (файлы раскрытой группы «Типов»).
    indent: f32,
    /// Вид строки фильтра анализа места.
    row: crate::du_view::RowKind,
    /// Размер раздела «Временных» — после названия.
    section: Option<SharedString>,
    /// Вкладка пакетов: тон колонок по порядку (пусто — как у файлов).
    tones: SmallVec<[crate::pkg_view::Tone; 4]>,
}

/// Отступ файлов раскрытой группы «Типов» (`screen-dutypes`).
const DU_INDENT: f32 = 20.0;
/// Отступ копий в группе «Дубликатов» (`screen-dudups`).
const DUP_INDENT: f32 = 22.0;
/// Треугольники группы «Типов»: раскрыта, свёрнута (`CaretDownSolid8`, `CaretRightSolid8`).
const GLYPH_OPEN: &str = "\u{EDDC}";
const GLYPH_SHUT: &str = "\u{EDDA}";

/// Колонки строки вкладки `du:` (F01-05).
#[derive(Clone)]
struct DuCell {
    /// Доля 0…1; меньше нуля — полосы нет (строки без файла).
    share: f32,
    pct: SharedString,
    partial: bool,
}

/// Геометрия строки в колонке: отступы и ширина колонок вида.
#[derive(Clone, Copy)]
struct Geo {
    row_h: f32,
    pad: f32,
    icon_w: f32,
    icon_size: f32,
    /// Все колонки вида (без ужатия под ширину).
    cols_w: f32,
}

/// То же правило колонок, что [`fit_cols`], для колонки списка шириной `w`: ширина
/// имени и места колонок.
fn fit_cols_geo(geo: &Geo, cols: &[Column], w: f32) -> (f32, SmallVec<[Slot; 8]>) {
    columns::layout(cols, w - ROW_MX - geo.pad - geo.name_x(), NAME_MIN)
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
}

/// Отступ строки от краёв колонки (`mx_1`).
const ROW_MX: f32 = 4.0;
/// Имя не уже этого: в узкой панели сначала уходит дата, потом размер (как в egui).
const NAME_MIN: f32 = 120.0;

/// Колонки, что влезают в панель шириной `w` (для шапки).
pub(super) fn fit_cols(theme: &Theme, cols: &[Column], w: f32) -> SmallVec<[Slot; 8]> {
    let pad = theme.m(Metric::PadRowX) - ROW_MX;
    let name_x = ROW_MX + pad + MARK_W + ICON_PX + ICON_GAP;
    columns::layout(cols, w - ROW_MX - pad - name_x, NAME_MIN).1
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
    /// Предупреждение в подсказке («нет доступа»).
    warn: Hsla,
    chip: Hsla,
    icons: bool,
    /// Наведение мыши (палитра B): фон `current_line` с долей перехода.
    hover_bg: Hsla,
    hover: Option<HoverPaint>,
}

/// Подсветка наведения на строку страницы: новая строка проявляется (`a_in`), прежняя
/// гаснет (`a_out`) — переход 83 мс (DESIGN «Движение»).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct HoverPaint {
    pub slot: Option<u32>,
    pub a_in: f32,
    pub prev: Option<u32>,
    pub a_out: f32,
}

impl HoverPaint {
    /// Отпечаток для кэша части панели (доли — с шагом 1/64).
    pub fn print(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::hash::DefaultHasher::new();
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "доля 0..=1"
        )]
        let q = |a: f32| (a.clamp(0.0, 1.0) * 64.0).round() as u8;
        (self.slot, self.prev, q(self.a_in), q(self.a_out)).hash(&mut h);
        h.finish() | 1
    }
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
    view_cols: &[Column],
    ext: Option<&dyn urfin_vfs::Vfs>,
    field: Option<&super::field::Field>,
    glide: Option<f32>,
    hover: Option<HoverPaint>,
    icon_mode: IconMode,
    hl: &Highlight,
    cx: &Listen,
) -> Stateful<Div> {
    let total = t.rows();
    let top = t.cursor.top;
    let rows = rows.max(1);
    let cols = cols.max(1);
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
        cols_w: view_cols.iter().map(|c| c.w).sum(),
    };
    let rename = super::field::rename_input(field, t);
    let dir_links = Some(&hl.links);
    let i18n = urfin_i18n::current();
    let dir_size = SharedString::from(i18n.get("ui-size-dir"));
    // Вкладка анализа места: знаменатель доли — сумма каталога.
    let du_total = crate::du_view::is_du(t).then(|| crate::du_view::dir_total(t));
    // Виды фильтров (F01-08): группы, отступы, папка перед именем.
    let du_temp = matches!(hl.du, Some((urfin_vfs::du::DuFilter::Temp, _)));
    // Вкладка пакетов: строки модели и «важное» (только в «Обновлениях»).
    let pkg_view = hl.pkg.as_deref();
    let badges = crate::pkg_view::filter(t) == Some(urfin_pkg::Filter::Updates);
    let important = SharedString::from(i18n.get("pkg-important"));
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
            // Строка пакета (F02-08): значок пакета, имя — основным текстом.
            Some(Row::Entry { idx, entry }) if pkg_view.is_some() => (
                GLYPH_PACKAGE,
                SharedString::from(entry.name.to_string()),
                t.selection.contains(idx),
                None,
                Some(entry),
            ),
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
        // Поиск пакетов (F02-09, `screen-pkgsearch`): группа программы — треугольник и
        // полужирно, варианты — с отступом, без значка.
        let search_row = pkg_view
            .zip(entry)
            .and_then(|(v, e)| crate::pkg_view::search_row(v, t, &e.name));
        let du_row = match (hl.du, entry, search_row) {
            (Some((f, open)), Some(e), _) => crate::du_view::row_kind_of(f, open, e),
            (_, _, Some(r)) => match r.kind {
                urfin_pkg::SearchKind::Group { open, .. } => {
                    crate::du_view::RowKind::Group { open }
                }
                urfin_pkg::SearchKind::Variant { .. } => crate::du_view::RowKind::Member,
            },
            _ => crate::du_view::RowKind::Plain,
        };
        // «Дубликаты» (`screen-dudups`): у копии — где лежит (папка, значок папки), у
        // заголовка — значок и цвет файла.
        let (glyph, name, color) = match (du_row, entry) {
            (crate::du_view::RowKind::DupCopy, Some(e)) => (
                crate::icons::glyph(icon_class(&e.name, urfin_core::EntryKind::Dir, e.attrs)),
                SharedString::from(crate::du_view::dup_where(
                    hl.du_root.as_deref().unwrap_or(""),
                    &e.name,
                )),
                None,
            ),
            (crate::du_view::RowKind::DupHead, Some(e)) => {
                let as_file = urfin_core::Entry {
                    kind: urfin_core::EntryKind::File,
                    ..e.clone()
                };
                let group = theme.hl.classify(&as_file);
                (
                    crate::icons::glyph(icon_class(&e.name, as_file.kind, e.attrs)),
                    name,
                    theme.hl.color(group, &theme.tokens),
                )
            }
            _ => (glyph, name, color),
        };
        let dup_copy = du_row == crate::du_view::RowKind::DupCopy;
        let pkg_row = pkg_view
            .zip(entry)
            .and_then(|(v, e)| crate::pkg_view::row(v, t, &e.name));
        let tones: SmallVec<[crate::pkg_view::Tone; 4]> = if let Some(r) = search_row {
            view_cols
                .iter()
                .map(|c| match c.kind {
                    ColKind::Scheme(x) => crate::pkg_view::search_tone(r, x.id),
                    _ => crate::pkg_view::Tone::Text,
                })
                .collect()
        } else {
            pkg_row
                .map(|r| {
                    view_cols
                        .iter()
                        .map(|c| match c.kind {
                            ColKind::Scheme(x) => crate::pkg_view::tone(r, x.id),
                            _ => crate::pkg_view::Tone::Text,
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
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
        let cells = view_cols
            .iter()
            .map(|c| match c.kind {
                // Копия дубликата: размер — у заголовка группы.
                ColKind::Size if dup_copy => None,
                ColKind::Size if link.is_some() => {
                    Some(SharedString::from(i18n.get(match link_info {
                        Some(l) if l.junction => "ui-size-junction",
                        Some(_) => "ui-size-symlink",
                        None => "ui-size-link",
                    })))
                }
                ColKind::Size
                    if dir
                        && entry.is_some_and(|e| !e.attrs.has(urfin_core::Attrs::SIZE_KNOWN)) =>
                {
                    Some(dir_size.clone())
                }
                // Колонка схемы (`Entry.ext`): данные backend'а из памяти.
                ColKind::Scheme(x) => {
                    let v = ext.zip(entry).and_then(|(v, e)| v.ext(&t.loc, e, x.id))?;
                    let mut c = Cell::new();
                    crate::fmt_cell::ext_text(&v, &theme.fmt, &mut c);
                    Some(SharedString::from(c.as_str().to_owned()))
                }
                kind => col(kind),
            })
            .collect();
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
        let glyph = match du_row {
            crate::du_view::RowKind::Group { open: true } => GLYPH_OPEN.to_owned(),
            crate::du_view::RowKind::Group { open: false } => GLYPH_SHUT.to_owned(),
            crate::du_view::RowKind::More | crate::du_view::RowKind::Section => String::new(),
            crate::du_view::RowKind::Member if search_row.is_some() => String::new(),
            _ => glyph.to_string(),
        };
        let virt = matches!(
            du_row,
            crate::du_view::RowKind::More | crate::du_view::RowKind::Section
        );
        let section = (du_row == crate::du_view::RowKind::Section)
            .then(|| entry.map(|e| SharedString::from(urfin_i18n::fmt::bytes_with(&i18n, e.size))))
            .flatten();
        let note = entry.and_then(|e| {
            if du_row == crate::du_view::RowKind::DupHead {
                hl.notes.get(&crate::du_view::dup_key(e)).cloned()
            } else {
                hl.notes.get(e.name.as_str()).cloned()
            }
        });
        // «Временные» (решение 2026-11-02 «du-filters» №4): понятное имя крупно, путь
        // от корня — серым рядом.
        let (name, tail) = match (&note, entry) {
            (Some(Note::Hint { label: Some(l), .. }), Some(e)) => {
                (SharedString::from(format!("{l}  {}", e.name)), l.len())
            }
            _ => (name, 0),
        };
        let dim = entry
            .filter(|e| e.kind != urfin_core::EntryKind::Other && tail == 0)
            .map_or(0, |e| {
                crate::du_view::dim_len_of(hl.du.map(|d| d.0), &e.name)
            });
        lines.push(Line {
            hl: hl_range,
            target: entry.map(|e| SharedString::from(e.name.to_string())),
            glyph: SharedString::from(glyph.clone()),
            cells: if du_row == crate::du_view::RowKind::Section {
                SmallVec::new()
            } else {
                cells
            },
            dim,
            tail,
            indent: match du_row {
                crate::du_view::RowKind::Member | crate::du_view::RowKind::More => DU_INDENT,
                crate::du_view::RowKind::DupCopy => DUP_INDENT,
                _ => 0.0,
            },
            row: du_row,
            section,
            name,
            strong: (dir && !dup_copy)
                || matches!(
                    du_row,
                    crate::du_view::RowKind::Group { .. }
                        | crate::du_view::RowKind::Section
                        | crate::du_view::RowKind::DupHead
                ),
            cursor: cursor.then_some(active),
            marked,
            color: if broken {
                theme.c(Role::Modified)
            } else if dup_copy {
                theme.c(Role::TextPrimary)
            } else {
                kind_color
            },
            icon_color: match icons {
                _ if pkg_view.is_some() => theme.c(Role::TextSecondary),
                IconMode::Color if dir || dup_copy => theme.c(if theme.vivid() {
                    Role::AccentFill
                } else {
                    Role::AccentText
                }),
                IconMode::Color => kind_color,
                _ => theme.c(Role::TextSecondary),
            },
            renaming,
            note: match (pkg_row, search_row.map(|r| &r.kind)) {
                (Some(r), _) if badges && r.important => Some(Note::Badge(important.to_string())),
                // Группа поиска: число вариантов в плашке после имени.
                (_, Some(urfin_pkg::SearchKind::Group { count, .. })) => {
                    Some(Note::Tag(count.to_string()))
                }
                _ => note,
            },
            tones,
            du: du_total.zip(entry).map(|(total, e)| {
                let (share, pct) = crate::du_view::share(e.size, total);
                DuCell {
                    share: if virt { -1.0 } else { share },
                    // Во «Временных» — только полоса (`screen-dutemp`).
                    pct: SharedString::from(if du_temp || virt { String::new() } else { pct }),
                    partial: crate::du_view::partial(e),
                }
            }),
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
        warn: theme.c(Role::CautionFg),
        chip: theme.c(Role::FillControl),
        icons: icons != IconMode::None,
        hover_bg: theme.c(Role::CurrentLine),
        hover,
    };
    let paint_lines = lines.clone();
    let paint_bounds = bounds.clone();
    let paint_cols: SmallVec<[Column; 8]> = view_cols.into();
    let canvas = canvas(
        move |b, _, _| paint_bounds.set(b),
        move |b, (), window, cx| {
            paint(
                &paint_lines,
                &paint_cols,
                b,
                rows,
                cols,
                geo,
                pal,
                glide,
                window,
                cx,
            );
        },
    )
    .size_full();
    // Слот страницы под точкой (`None` снаружи — мимо строк).
    let slot_at = move |p: Point<Pixels>| -> Option<u32> {
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
        Some(c.min(cols - 1) * rows + r)
    };
    // Строка под точкой → цель для `panel.cursor.to`.
    let hover_at = slot_at.clone();
    let hit = move |lines: &[Line], p: Point<Pixels>| -> Option<Option<SharedString>> {
        lines.get(slot_at(p)? as usize).map(|l| l.target.clone())
    };
    let n_lines = u32::try_from(lines.len()).unwrap_or(u32::MAX);
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
            if ev.pressed_button.is_none() {
                // Наведение (палитра B) — только мышь без кнопок.
                h.row_hover(side, hover_at(ev.position).filter(|k| *k < n_lines), cx);
            }
            if ev.pressed_button != Some(MouseButton::Left) {
                h.right_drag_end();
            } else if let Some(target) = move_hit(&move_lines, ev.position) {
                h.row_right_drag(side, target.as_deref(), w, cx);
            }
        }))
        .on_hover(cx.listener(move |h, over: &bool, _, cx| {
            if !*over {
                h.row_hover(side, None, cx);
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
                .pr(px(ROW_MX + geo.pad + geo.cols_w))
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
    view_cols: &[Column],
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
    // Права `drwxr-xr-x` у `sftp:` — моно (`screen-sftppanel`).
    let mono = gpui_kit::font("Cascadia Mono");
    let strong = Font {
        weight: gpui_kit::FontWeight::MEDIUM,
        ..font.clone()
    };
    let icons = gpui_kit::font(ICON_FONT);
    let row_h = px(geo.row_h);
    #[allow(clippy::cast_precision_loss, reason = "колонок единицы")]
    let col_w = f32::from(b.size.width) / cols as f32;
    let (name_w, slots) = fit_cols_geo(&geo, view_cols, col_w);
    let name_w = px(name_w);
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
        // Наведение мыши (палитра B): `current_line` под отмеченными и курсором.
        if let Some(hv) = pal.hover {
            for (slot, a) in [(hv.prev, hv.a_out), (hv.slot, hv.a_in)] {
                let Some(k) = slot.map(|k| k as usize).filter(|k| *k < lines.len()) else {
                    continue;
                };
                if a <= 0.0 || lines[k].marked {
                    continue;
                }
                let (x0, y) = at(k);
                let row = Bounds::new(point(x0, y), size2(px(col_w), row_h));
                window.paint_quad(gpui_kit::fill(row, pal.hover_bg.opacity(a)));
            }
        }
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
            // Отступ (файлы раскрытой группы) — у иконки и имени, колонки на месте.
            let row_x = x0;
            let x0 = x0 + px(l.indent);
            let name_w = name_w - px(l.indent);
            if pal.icons && !l.glyph.is_empty() {
                // Треугольник группы — мельче иконки, вторичным цветом.
                let (glyph_px, glyph_color) = match l.row {
                    crate::du_view::RowKind::Group { .. } => (8.0, secondary),
                    _ => (geo.icon_size, l.icon_color),
                };
                let icon = shape(window, &l.glyph, &icons, glyph_color, px(glyph_px));
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
                    _ if l.dim > 0 && l.dim < l.name.len() => vec![
                        run(l.dim, &font, secondary),
                        run(l.name.len() - l.dim, f, l.color),
                    ],
                    _ if l.tail > 0 && l.tail < l.name.len() => vec![
                        run(l.tail, f, l.color),
                        run(l.name.len() - l.tail, &font, secondary),
                    ],
                    _ if l.row == crate::du_view::RowKind::More => {
                        vec![run(l.name.len(), &font, secondary)]
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
                        .truncate_line(
                            l.name.clone(),
                            room,
                            "…",
                            &runs,
                            // Где лежит копия дубликата — важен конец пути.
                            if l.row == crate::du_view::RowKind::DupCopy {
                                TruncateFrom::Start
                            } else {
                                TruncateFrom::End
                            },
                        );
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
                    let limit = x0 + px(geo.name_x()) + name_w;
                    paint_note(note, end, limit, y, row_h, &font, size, pal, window, cx);
                }
                // Раздел «Временных»: размер раздела — после названия (`screen-dutemp`).
                if let Some(total) = &l.section {
                    let end = x0 + px(geo.name_x()) + line.width.min(name_w) + px(8.0);
                    let t = shape(window, total, &font, secondary, size);
                    let _ = t.paint(point(end, y), row_h, TextAlign::Left, None, window, cx);
                }
            }
            // Колонки вплотную к правому краю: размер и даты прижаты вправо, прочие —
            // влево с отступом и обрезаются «…».
            let cols_x = row_x + px(geo.name_x());
            for (i, (slot, s)) in slots.iter().zip(&l.cells).enumerate() {
                // Доля в папке (F01-05): полоса 120×6 `accent` на `current_line` и процент;
                // недосчитанная — серая.
                if slot.kind == ColKind::Share {
                    if let Some(du) = l.du.as_ref().filter(|d| d.share >= 0.0) {
                        let x = cols_x + px(slot.x + columns::GAP);
                        let track = Bounds::new(
                            point(x, y + (row_h - px(6.0)) / 2.0),
                            size2(px(120.0), px(6.0)),
                        );
                        window
                            .paint_quad(gpui_kit::fill(track, pal.hover_bg).corner_radii(px(3.0)));
                        if du.share > 0.0 {
                            let w = px((120.0 * du.share).max(3.0));
                            window.paint_quad(
                                gpui_kit::fill(
                                    Bounds::new(track.origin, size2(w, px(6.0))),
                                    if du.partial { secondary } else { pal.accent },
                                )
                                .corner_radii(px(3.0)),
                            );
                        }
                        let pct = shape(window, &du.pct, &font, secondary, size);
                        let px_x = cols_x + px(slot.x + slot.w) - pct.width;
                        let _ = pct.paint(point(px_x, y), row_h, TextAlign::Left, None, window, cx);
                    }
                    continue;
                }
                let Some(s) = s else {
                    continue;
                };
                // «рекомендуется» — метка в рамке `accent` (`screen-pkgsearch`).
                if l.tones.get(i).copied() == Some(crate::pkg_view::Tone::Badge) {
                    let x = cols_x + px(slot.x + columns::GAP);
                    paint_badge(s, Tone::Ok, x, y, row_h, &font, pal, window, cx);
                    continue;
                }
                // У вкладки анализа размер — основной текст, недосчитанный «≥ X» — вторичный.
                // У вкладки пакетов: «Доступна» — `accent`, «вне менеджера» — `secondary`.
                let tone = l.tones.get(i).copied();
                let color = match (&l.du, tone) {
                    (_, Some(crate::pkg_view::Tone::Accent)) => pal.accent_text,
                    (_, Some(crate::pkg_view::Tone::Text)) => pal.text,
                    (Some(du), _) if slot.kind == ColKind::Size && !du.partial => pal.text,
                    _ => secondary,
                };
                let font = match slot.kind {
                    _ if tone == Some(crate::pkg_view::Tone::Accent) => &strong,
                    ColKind::Scheme(c) if c.mono => &mono,
                    _ => &font,
                };
                let mut line = shape(window, s, font, color, size);
                let x = if slot.kind.right_aligned() {
                    cols_x + px(slot.x + slot.w) - line.width
                } else {
                    let room = px((slot.w - columns::GAP).max(0.0));
                    if line.width > room {
                        let runs = [run(s.len(), font, color)];
                        let (cut, runs) = cx
                            .text_system()
                            .line_wrapper(font.clone(), size)
                            .truncate_line(s.clone(), room, "…", &runs, TruncateFrom::End);
                        line = window.text_system().shape_line(cut, size, &runs, None);
                    }
                    cols_x + px(slot.x + columns::GAP)
                };
                let _ = line.paint(point(x, y), row_h, TextAlign::Left, None, window, cx);
            }
        }
    });
}
