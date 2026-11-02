//! Карта анализа места во второй панели (F01-07, F01 §3.4, `screen-dumap`): пока
//! активна вкладка `du:` и включена карта (`du.map`, Ctrl+Tab), пассивная панель вместо
//! списка рисует карту папки списка — плитки quads одним `canvas`.
//!
//! Раскладка — `crate::treemap` с кэшем на 8 раскладок (ключ: каталог, номер изменения
//! снимка, размер, масштаб); курсор, наведение, отметка и «вне фильтра» — слой поверх,
//! без перераскладки. Клик по плитке — курсор на строку списка, двойной — внутрь,
//! Shift+клик — отметка строки. Простой — без перерисовок (анимаций нет).

use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::rc::Rc;

use gpui_kit::prelude::*;
use gpui_kit::{
    Background, Bounds, ContentMask, Div, Font, Hsla, MouseButton, MouseMoveEvent, MouseUpEvent,
    Pixels, SharedString, TextAlign, TextRun, canvas, div, point, px, size as size2,
};
use rustc_hash::FxHashSet;
use urfin_core::{AppState, Command, EntryKind, Row, Side, Tab};
use urfin_vfs::du::{DuFilter, DuType, MapItem, MapKind, dvfs};

use super::panel::{HoverPaint, Listen};
use super::theme::Theme;
use crate::du_view;
use crate::tokens::{Metric, Role};
use crate::treemap::{self, Cache, Key, Rect, Tile, TileKind};

/// Строка легенды под картой.
const LEGEND_H: f32 = 30.0;
/// Подпись плитки: кегль, строка, отступ.
const LABEL: f32 = 12.0;
const LINE_H: f32 = 15.0;
const PAD: f32 = 4.0;
/// Плотность заливки: подложка каталога с детьми, плитка, «вне фильтра» (доля).
const A_HEAD: f32 = 0.16;
const A_TILE: f32 = 0.42;
const A_OUT: f32 = 0.35;

thread_local! {
    /// Раскладки карты (UI-поток).
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
}

/// Сторона, где сейчас карта: вторая панель при активной вкладке `du:` и включённой
/// карте.
pub(super) fn side(s: &AppState) -> Option<Side> {
    if !dvfs::map_on(s) || !du_view::is_du(s.active_tab()) {
        return None;
    }
    let other = s.active.other();
    s.panel(other).visible.then_some(other)
}

/// Таб `t` сейчас закрыт картой (его панель — вторая).
pub(super) fn shows(s: &AppState, t: &Tab) -> bool {
    side(s).is_some_and(|o| s.panel(o).tab().id == t.id)
}

/// Отпечаток карты для кэша кадра стороны `at`: вкладка анализа, курсор, отметка, номер
/// изменения снимка. Не карта — `None`.
pub(super) fn print(s: &AppState, at: Side) -> Option<u64> {
    if side(s) != Some(at) {
        return None;
    }
    let t = s.active_tab();
    let mut h = std::hash::DefaultHasher::new();
    (
        format!("{:?}", t.loc),
        t.rev,
        t.cursor.row,
        t.selection.len(),
        t.selection.bytes(),
        du_view::rev(s),
    )
        .hash(&mut h);
    du_view::look(s, t).map(|(f, o)| (f.id(), o)).hash(&mut h);
    Some(h.finish())
}

/// Шапка карты: «Карта · C:\Dev».
pub(super) fn head(s: &AppState, side: Side, theme: &Theme) -> Div {
    let t = s.active_tab();
    let dir = du_view::info(s, t).map_or_else(String::new, |i| i.dir.display().to_string());
    let title = format!("{} · {dir}", urfin_i18n::current().get("du-map-title"));
    super::views::head(title, String::new(), s.active == side, theme)
}

/// Пункт легенды плитки: 0 папки, 1 архивы и образы, 2 исполняемые, 3 временные,
/// 4 прочее.
fn class(kind: TileKind) -> usize {
    match kind {
        TileKind::Dir => 0,
        TileKind::File(DuType::Archive | DuType::DiskImage) => 1,
        TileKind::File(DuType::Exec) => 2,
        TileKind::File(DuType::Temp) => 3,
        _ => 4,
    }
}

const LEGEND: [&str; 5] = [
    "du-map-dirs",
    "du-map-archives",
    "du-map-exec",
    "du-map-temp",
    "du-map-other",
];

/// Цвета классов: каталоги — `accent`, типы — роли подсветки файлов (№32), прочее
/// серое.
fn tints(theme: &Theme) -> [Hsla; 5] {
    [
        theme.c(Role::AccentFill),
        theme.c(Role::TypeArchive),
        theme.c(Role::TypeExec),
        theme.c(Role::TypeTemp),
        theme.c(Role::TextTertiary),
    ]
}

fn item_class(m: &MapItem) -> usize {
    class(match m.kind {
        MapKind::Dir => TileKind::Dir,
        MapKind::File(t) => TileKind::File(t),
        MapKind::Small { .. } => TileKind::Small,
    })
}

/// Что в фильтре вкладки (плитки вне — бледнее): «Типы» — файлы группы под курсором,
/// «Временные» — совпадения правил; прочие виды — всё.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    All,
    Type(DuType),
    Temp,
}

fn focus(t: &Tab) -> Focus {
    match du_view::filter(t) {
        Some(DuFilter::Temp) => Focus::Temp,
        Some(DuFilter::Types) => {
            let ty = match t.current() {
                Some(Row::Entry { entry, .. }) if entry.kind == EntryKind::Other => {
                    match urfin_vfs::du::TypeGroup::from_label(&entry.name) {
                        Some(urfin_vfs::du::TypeGroup::Type(t)) => Some(t),
                        _ => None,
                    }
                }
                Some(Row::Entry { entry, .. }) => {
                    Some(DuType::of(&urfin_vfs::walk::RawName::Str {
                        name: &entry.name,
                        escaped: false,
                    }))
                }
                _ => None,
            };
            ty.map_or(Focus::All, Focus::Type)
        }
        _ => Focus::All,
    }
}

fn in_focus(f: Focus, tile: &Tile) -> bool {
    match f {
        Focus::All => true,
        Focus::Temp => tile.temp,
        Focus::Type(ty) => tile.kind == TileKind::File(ty),
    }
}

/// Строка списка для плитки: в «По папкам» — плитка первого уровня (у второго — её
/// папка), в плоских видах — путь плитки.
fn row_of(flat: bool, tile: &Tile) -> Option<String> {
    if tile.path.is_empty() {
        return None;
    }
    Some(if flat {
        tile.path.to_string()
    } else {
        tile.path.split('\\').next().unwrap_or_default().to_owned()
    })
}

/// Цвета слоя поверх плиток.
#[derive(Clone, Copy)]
struct Pal {
    panel: Hsla,
    text: Hsla,
    secondary: Hsla,
    accent: Hsla,
    hover: Hsla,
}

/// Что рисует кадр: слой поверх раскладки.
struct Over {
    marked: FxHashSet<String>,
    cursor: Option<String>,
    focus: Focus,
    flat: bool,
    hover: Option<u32>,
    tints: [Hsla; 5],
    pal: Pal,
    more: String,
    small: String,
}

/// Последняя отрисованная раскладка: начало карты и плитки (для мыши).
#[derive(Default)]
struct Shown {
    origin: (f32, f32),
    tiles: Rc<Vec<Tile>>,
}

/// Карта: плитки и легенда. Пока считается — подсказка вместо карты.
#[allow(clippy::too_many_lines, reason = "данные, слой поверх, мышь, легенда")]
pub(super) fn body(
    s: &AppState,
    side: Side,
    theme: &Theme,
    hover: Option<HoverPaint>,
    cx: &Listen,
) -> Div {
    let du_side = side.other();
    let t = s.panel(du_side).tab();
    let i18n = urfin_i18n::current();
    let (Some(reg), Some(info)) = (dvfs::registry(s).cloned(), du_view::info(s, t)) else {
        return div();
    };
    // Каталог карты: в «По папкам» — каталог списка, в видах фильтров — корень анализа.
    let rel = info
        .dir
        .strip_prefix(&info.root)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let base = dvfs::location(info.id, &rel);
    let Some(data) = reg.map(&base) else {
        return div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme.c(Role::TextSecondary))
            .child(SharedString::from(i18n.get("du-map-scanning")));
    };
    let mut hasher = std::hash::DefaultHasher::new();
    (info.id, &rel).hash(&mut hasher);
    let dir_key = hasher.finish();
    let rev = data.rev;
    let flat = !matches!(du_view::filter(t), Some(DuFilter::Dirs) | None);
    let tints = tints(theme);
    let pal = Pal {
        panel: theme.c(Role::LayerPanel),
        text: theme.c(Role::TextPrimary),
        secondary: theme.c(Role::TextSecondary),
        accent: theme.c(Role::AccentFill),
        hover: theme.c(Role::CurrentLine),
    };
    let over = Over {
        marked: t
            .selection
            .iter()
            .filter_map(|i| t.listing.entries.get(i as usize))
            .map(|e| e.name.to_string())
            .collect(),
        cursor: match t.current() {
            Some(Row::Entry { entry, .. }) => Some(entry.name.to_string()),
            _ => None,
        },
        focus: focus(t),
        flat,
        hover: hover.and_then(|h| h.slot),
        tints,
        pal,
        more: i18n.get("du-map-more"),
        small: i18n.get("du-map-small"),
    };
    // Легенда — по элементам карты (оба уровня).
    let mut seen = [false; 5];
    for m in data
        .items
        .iter()
        .chain(data.items.iter().flat_map(|m| &m.kids))
    {
        seen[item_class(m)] = true;
    }
    let focus_all = over.focus == Focus::All;
    let items = Rc::new(data.items);
    let shown: Rc<RefCell<Shown>> = Rc::default();
    let paint_shown = Rc::clone(&shown);
    let map = canvas(
        |_, _, _| {},
        move |b: Bounds<Pixels>, (), window, cx| {
            let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
            let key = Key::new(dir_key, rev, w, h, window.scale_factor());
            let tiles = CACHE.with(|c| {
                c.borrow_mut().get_or(key, || {
                    let _span = tracing::debug_span!(target: "ui.du", "du.layout").entered();
                    treemap::layout(&items, Rect::new(0.0, 0.0, w, h))
                })
            });
            *paint_shown.borrow_mut() = Shown {
                origin: (f32::from(b.origin.x), f32::from(b.origin.y)),
                tiles: Rc::clone(&tiles),
            };
            window.with_content_mask(Some(ContentMask { bounds: b }), |window| {
                paint(&tiles, b, &over, window, cx);
            });
        },
    )
    .size_full();
    let hit_at = move |x: f32, y: f32| -> Option<(usize, Tile)> {
        let s = shown.borrow();
        let k = treemap::hit(&s.tiles, x - s.origin.0, y - s.origin.1)?;
        Some((k, s.tiles[k].clone()))
    };
    let move_hit = hit_at.clone();
    let du_name = super::chrome::side_name(du_side);
    let up = cx.listener(move |h, ev: &MouseUpEvent, w, cx| {
        let Some((_, tile)) = hit_at(f32::from(ev.position.x), f32::from(ev.position.y)) else {
            return;
        };
        let mut cmds = vec![Command::new("panel.focus").arg("side", du_name)];
        if let Some(row) = row_of(flat, &tile) {
            cmds.push(Command::new("panel.cursor.to").arg("name", row));
        }
        if ev.modifiers.shift {
            cmds.push(Command::new("sel.toggle"));
        } else if ev.click_count >= 2 {
            match tile.kind {
                // Двойной клик по папке — внутрь неё («По папкам»).
                TileKind::Dir => {
                    let mut loc = base.clone();
                    loc.path = loc.path.join(tile.path.as_str());
                    cmds.push(
                        Command::new("panel.go")
                            .arg("side", du_name)
                            .arg("path", loc),
                    );
                }
                TileKind::File(_) => cmds.push(Command::new("panel.enter")),
                _ => {}
            }
        }
        h.run_all(cmds, w, cx);
    });
    let mv = cx.listener(move |h, ev: &MouseMoveEvent, _, cx| {
        let k = move_hit(f32::from(ev.position.x), f32::from(ev.position.y))
            .and_then(|(k, _)| u32::try_from(k).ok());
        h.row_hover(side, k, cx);
    });
    let leave = cx.listener(move |h, over: &bool, _, cx| {
        if !*over {
            h.row_hover(side, None, cx);
        }
    });
    let swatch = |c: Background| div().size(px(12.0)).flex_none().rounded(px(2.0)).bg(c);
    let entry = |sw: Div, key: &str| {
        div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(sw)
            .child(SharedString::from(i18n.get(key)))
    };
    let mut legend = div()
        .h(px(LEGEND_H))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(16.0))
        .px(px(theme.m(Metric::PadRowX)))
        .text_size(px(12.0))
        .text_color(theme.c(Role::TextSecondary));
    for (k, key) in LEGEND.iter().enumerate().filter(|(k, _)| seen[*k]) {
        legend = legend.child(entry(swatch(tints[k].opacity(A_TILE).into()), key));
    }
    // «Отмечено» (штриховка) — при отметке; в «Типах» и «Временных» — «Вне фильтра».
    if !focus_all {
        legend = legend.child(entry(
            swatch(tints[0].opacity(A_TILE * A_OUT).into()),
            "du-map-out",
        ));
    } else if !t.selection.is_empty() {
        legend = legend.child(entry(
            swatch(gpui_kit::pattern_slash(pal.accent.opacity(0.55), 2.0, 4.0)),
            "du-map-marked",
        ));
    }
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .child(
            div()
                .id(SharedString::from(format!(
                    "du-map-{}",
                    super::chrome::side_name(side)
                )))
                .flex_1()
                .min_h_0()
                .mx(px(8.0))
                .mt(px(8.0))
                .relative()
                .overflow_hidden()
                .child(map)
                .on_mouse_up(MouseButton::Left, up)
                .on_mouse_move(mv)
                .on_hover(leave),
        )
        .child(legend)
}

/// Плитки, затем курсор поверх.
fn paint(
    tiles: &[Tile],
    b: Bounds<Pixels>,
    o: &Over,
    window: &mut gpui_kit::Window,
    cx: &mut gpui_kit::App,
) {
    let font = window.text_style().font();
    let strong = Font {
        weight: gpui_kit::FontWeight::MEDIUM,
        ..font.clone()
    };
    let at = |r: &Rect| {
        Bounds::new(
            point(b.origin.x + px(r.x), b.origin.y + px(r.y)),
            size2(px(r.w), px(r.h)),
        )
    };
    let i18n = urfin_i18n::current();
    for (k, tile) in tiles.iter().enumerate() {
        let r = at(&tile.rect);
        // Каталог с детьми — светлая подложка с полосой подписи, дети — плотнее.
        let mut a = if tile.head { A_HEAD } else { A_TILE };
        if !in_focus(o.focus, tile) {
            a *= A_OUT;
        }
        window.paint_quad(gpui_kit::fill(r, o.tints[class(tile.kind)].opacity(a)));
        if o.hover == u32::try_from(k).ok() {
            window.paint_quad(gpui_kit::fill(r, o.pal.hover.opacity(0.6)));
        }
        let marked = o.marked.contains(tile.path.as_str())
            || (tile.depth == 1 && row_of(o.flat, tile).is_some_and(|n| o.marked.contains(&n)));
        if marked {
            window.paint_quad(gpui_kit::fill(
                r,
                gpui_kit::pattern_slash(o.pal.accent.opacity(0.55), 2.0, 6.0),
            ));
        }
        window.paint_quad(gpui_kit::outline(
            r,
            o.pal.panel,
            gpui_kit::BorderStyle::Solid,
        ));
        let size = urfin_i18n::fmt::bytes_with(&i18n, tile.size);
        label(tile, r, &size, &font, &strong, o, window, cx);
    }
    // Курсор списка — рамка 2 px `text`: плитка строки (первый уровень или точный путь).
    let Some(c) = &o.cursor else {
        return;
    };
    let Some(tile) = tiles.iter().rev().find(|t| {
        t.path.as_str() == c.as_str() && matches!(t.kind, TileKind::Dir | TileKind::File(_))
    }) else {
        return;
    };
    let r = at(&tile.rect);
    window.paint_quad(gpui_kit::outline(
        r,
        o.pal.text,
        gpui_kit::BorderStyle::Solid,
    ));
    let inner = Bounds::new(
        point(r.origin.x + px(1.0), r.origin.y + px(1.0)),
        size2(r.size.width - px(2.0), r.size.height - px(2.0)),
    );
    window.paint_quad(gpui_kit::outline(
        inner,
        o.pal.text,
        gpui_kit::BorderStyle::Solid,
    ));
}

/// Подпись, где помещается: «имя · размер» строкой; не влезает — имя и размер двумя
/// строками (не у каталога с полосой подписи); и так нет — без подписи.
#[allow(clippy::too_many_arguments, reason = "плоский вызов отрисовки")]
fn label(
    tile: &Tile,
    r: Bounds<Pixels>,
    size: &str,
    font: &Font,
    strong: &Font,
    o: &Over,
    window: &mut gpui_kit::Window,
    cx: &mut gpui_kit::App,
) {
    let (w, h) = (f32::from(r.size.width), f32::from(r.size.height));
    if w < 36.0 || h < LINE_H + 2.0 {
        return;
    }
    let name = match tile.kind {
        TileKind::Rest { count } => format!("{} {count}", o.more),
        TileKind::Small => o.small.clone(),
        _ => tile.name.to_string(),
    };
    let run = |len: usize, f: &Font, color: Hsla| TextRun {
        len,
        font: f.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let name_f = if tile.depth == 1 || matches!(tile.kind, TileKind::Rest { .. }) {
        strong
    } else {
        font
    };
    let one = format!("{name} · {size}");
    let runs = [
        run(name.len(), name_f, o.pal.text),
        run(one.len() - name.len(), font, o.pal.secondary),
    ];
    let line = window
        .text_system()
        .shape_line(SharedString::from(one), px(LABEL), &runs, None);
    let room = w - 2.0 * PAD;
    let origin = |dy: f32| point(r.origin.x + px(PAD), r.origin.y + px(1.0 + dy));
    if f32::from(line.width) <= room {
        let _ = line.paint(origin(0.0), px(LINE_H), TextAlign::Left, None, window, cx);
        return;
    }
    if tile.head || h < 2.0 * LINE_H + 2.0 {
        return;
    }
    let name_line = window.text_system().shape_line(
        SharedString::from(name.clone()),
        px(LABEL),
        &[run(name.len(), name_f, o.pal.text)],
        None,
    );
    if f32::from(name_line.width) > room {
        return;
    }
    let size_line = window.text_system().shape_line(
        SharedString::from(size.to_owned()),
        px(LABEL),
        &[run(size.len(), font, o.pal.secondary)],
        None,
    );
    let _ = name_line.paint(origin(0.0), px(LINE_H), TextAlign::Left, None, window, cx);
    let _ = size_line.paint(
        origin(LINE_H),
        px(LINE_H),
        TextAlign::Left,
        None,
        window,
        cx,
    );
}
