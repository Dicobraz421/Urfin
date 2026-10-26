//! Сетка терминала на GPUI (S6-06, S6 §2): снапшот экрана рисует один `canvas`.
//! Ячейка — `cw` (ширина «M» моноширинного шрифта) × `ch` (высота строки), всё по
//! сетке: фоны — прямоугольники по runs, текст — runs одного цвета и начертания,
//! обрываемые на широком символе и на процедурных глифах. Рамки U+2500–257F и блоки
//! U+2580–259F рисуются прямоугольниками по ячейке (стыкуются без щелей), стрелки
//! Powerline U+E0B0–E0B3 — путями; остальное — шрифтом с запасными системными (эмодзи,
//! CJK). Курсор — блок, черта или подчёркивание. Выделение (S6-07) — полупрозрачные
//! прямоугольники поверх ячеек.

use std::rc::Rc;

use gpui_kit::prelude::*;
use gpui_kit::{
    App, Bounds, Context, Font, Hsla, PathBuilder, Pixels, SharedString, TextAlign, TextRun,
    Window, canvas, point, px, size as size2,
};
use urfin_core::{AppState, Args, Command};
use urfin_term::vt::grid::{Cell, CursorKind, Snapshot};

use super::CoreHost;
use super::features::{Frame, Main, ShellFeature};
use super::screens::Scene;
use super::theme::Theme;
use crate::tokens::Metric;

/// Моноширинный шрифт терминала (S6, системный Cascadia Mono).
const MONO: &str = "Cascadia Mono";
/// Поля вокруг сетки, px.
pub(super) const PAD_X: f32 = 8.0;
pub(super) const PAD_Y: f32 = 4.0;

pub(super) fn hsla([r, g, b]: [u8; 3]) -> Hsla {
    gpui_kit::Rgba {
        r: f32::from(r) / 255.0,
        g: f32::from(g) / 255.0,
        b: f32::from(b) / 255.0,
        a: 1.0,
    }
    .into()
}

/// Цвета терминала по умолчанию под тему (тёмная — Campbell).
pub(super) fn defaults(dark: bool) -> urfin_term::vt::grid::Defaults {
    if dark {
        urfin_term::vt::grid::Defaults {
            fg: [0xcc, 0xcc, 0xcc],
            bg: [0x0c, 0x0c, 0x0c],
            cursor: [0xff, 0xff, 0xff],
        }
    } else {
        urfin_term::vt::grid::Defaults {
            fg: [0x1f, 0x1f, 0x1f],
            bg: [0xfa, 0xfa, 0xfa],
            cursor: [0x1f, 0x1f, 0x1f],
        }
    }
}

/// Вид transcript (S6 §3.2): строки выше текущего промпта (`prompt_row`), прижатые к
/// низу области в `fit` строк, — прямо над cmdline; сам промпт и курсор — в cmdline.
/// После `cls` промпт в первой строке — область пуста, строка ввода внизу.
pub(super) fn transcript(snap: &Snapshot, prompt_row: usize, fit: usize) -> Snapshot {
    let n = prompt_row.min(snap.cells.len());
    let first = n.saturating_sub(fit);
    let pad = fit - (n - first);
    // Пустые строки сверху — прижать вывод к низу.
    let mut cells = vec![Vec::new(); pad];
    cells.extend(snap.cells[first..n].iter().cloned());
    // Выделение — в строки вида transcript.
    let selection = snap
        .selection
        .iter()
        .filter(|(r, ..)| (first..n).contains(&usize::from(*r)))
        .filter_map(|&(r, a, b)| Some((u16::try_from(usize::from(r) - first + pad).ok()?, a, b)))
        .collect();
    Snapshot {
        rows: u16::try_from(cells.len()).unwrap_or(u16::MAX),
        cells,
        cursor_kind: CursorKind::Hidden,
        selection,
        ..snap.clone()
    }
}

/// Сдвиг строк transcript: строка вида `r` — строка экрана `r + shift` (мышь, S6-07).
pub(super) fn transcript_shift(rows: usize, prompt_row: usize, fit: usize) -> isize {
    let n = prompt_row.min(rows);
    let first = n.saturating_sub(fit);
    let pad = fit - (n - first);
    first.cast_signed() - pad.cast_signed()
}

/// Глиф рисуется процедурно (рамки, блоки, стрелки Powerline).
fn procedural(c: char) -> bool {
    matches!(c as u32, 0x2500..=0x259F | 0xE0B0..=0xE0B3)
}

/// Где нарисована сетка (окно, px): мышь переводит позицию в ячейку.
pub(super) type GridBounds = Rc<std::cell::Cell<Option<Bounds<Pixels>>>>;

/// Сетка снапшота во всю площадь элемента; `text_px` — размер моноширинного шрифта.
pub(super) fn view(snap: Rc<Snapshot>, text_px: f32, at: GridBounds) -> impl IntoElement {
    let bg = hsla(snap.bg);
    canvas(
        |_, _, _| {},
        move |b, (), window, cx| {
            at.set(Some(b));
            paint(&snap, text_px, b, bg, window, cx);
        },
    )
    .size_full()
}

/// Ячейка под точкой окна: (строка вида, колонка, правая половина ячейки).
pub(super) fn cell_at(
    b: Bounds<Pixels>,
    p: gpui_kit::Point<Pixels>,
    (cw, ch): (Pixels, Pixels),
) -> (i64, i64, bool) {
    let x = (f32::from(p.x) - f32::from(b.origin.x) - PAD_X) / f32::from(cw);
    let y = (f32::from(p.y) - f32::from(b.origin.y) - PAD_Y) / f32::from(ch);
    #[allow(clippy::cast_possible_truncation, reason = "ячеек в окне — до тысяч")]
    let (col, row) = (x.floor().max(0.0) as i64, y.floor().max(0.0) as i64);
    (row, col, x - x.floor() >= 0.5)
}

/// Метрики ячейки (S6 §2): ширина «M» и высота строки, целые пиксели.
pub(super) fn metrics(text_px: f32, window: &mut Window) -> (Pixels, Pixels) {
    let font = gpui_kit::font(MONO);
    let run = TextRun {
        len: 1,
        font,
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let w = window
        .text_system()
        .shape_line(SharedString::new_static("M"), px(text_px), &[run], None)
        .width;
    (
        px(f32::from(w).round().max(1.0)),
        px((text_px * 1.3).round()),
    )
}

#[allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "фоны, текст, глифы, курсор — один проход по ячейкам"
)]
fn paint(
    snap: &Snapshot,
    text_px: f32,
    b: Bounds<Pixels>,
    bg: Hsla,
    window: &mut Window,
    cx: &mut App,
) {
    let started = std::time::Instant::now();
    window.paint_quad(gpui_kit::fill(b, bg));
    let font = gpui_kit::font(MONO);
    let (cw, ch) = metrics(text_px, window);
    let origin = b.origin + point(px(PAD_X), px(PAD_Y));
    let at = |row: usize, col: usize| {
        #[allow(clippy::cast_precision_loss, reason = "номер ячейки")]
        let p = point(origin.x + cw * col as f32, origin.y + ch * row as f32);
        p
    };
    window.with_content_mask(Some(gpui_kit::ContentMask { bounds: b }), |window| {
        for (r, line) in snap.cells.iter().enumerate() {
            // Фоны: runs одного цвета, отличного от фона экрана.
            let mut c = 0;
            while c < line.len() {
                let cbg = line[c].bg;
                let mut e = c + 1;
                while e < line.len() && line[e].bg == cbg {
                    e += 1;
                }
                if cbg != snap.bg {
                    #[allow(clippy::cast_precision_loss, reason = "ширина run")]
                    let w = cw * (e - c) as f32;
                    window.paint_quad(gpui_kit::fill(
                        Bounds::new(at(r, c), size2(w, ch)),
                        hsla(cbg),
                    ));
                }
                c = e;
            }
            // Текст: runs одного цвета и начертания; широкие и процедурные — отдельно.
            let mut c = 0;
            while c < line.len() {
                let cell = line[c];
                if cell.spacer || cell.ch == ' ' && !cell.underline && !cell.strike {
                    c += 1;
                    continue;
                }
                if procedural(cell.ch) {
                    box_glyph(cell.ch, at(r, c), cw, ch, hsla(cell.fg), window);
                    c += 1;
                    continue;
                }
                let mut e = c + 1;
                let same = |x: &Cell| {
                    x.fg == cell.fg
                        && x.bold == cell.bold
                        && x.italic == cell.italic
                        && x.underline == cell.underline
                        && x.strike == cell.strike
                        && !x.wide
                        && !x.spacer
                        && !procedural(x.ch)
                };
                if !cell.wide {
                    while e < line.len() && same(&line[e]) {
                        e += 1;
                    }
                }
                let text: String = line[c..e].iter().map(|x| x.ch).collect();
                let f = Font {
                    weight: if cell.bold {
                        gpui_kit::FontWeight::BOLD
                    } else {
                        gpui_kit::FontWeight::NORMAL
                    },
                    style: if cell.italic {
                        gpui_kit::FontStyle::Italic
                    } else {
                        gpui_kit::FontStyle::Normal
                    },
                    ..font.clone()
                };
                let color = hsla(cell.fg);
                let run = TextRun {
                    len: text.len(),
                    font: f,
                    color,
                    background_color: None,
                    underline: cell.underline.then(|| gpui_kit::UnderlineStyle {
                        thickness: px(1.0),
                        color: Some(color),
                        wavy: false,
                    }),
                    strikethrough: cell.strike.then(|| gpui_kit::StrikethroughStyle {
                        thickness: px(1.0),
                        color: Some(color),
                    }),
                };
                let shaped = window.text_system().shape_line(
                    SharedString::from(text),
                    px(text_px),
                    &[run],
                    None,
                );
                let mut p = at(r, c);
                if cell.wide {
                    // CJK и эмодзи — по центру своих двух ячеек.
                    p.x += ((cw * 2.0 - shaped.width) / 2.0).max(px(0.0));
                }
                let _ = shaped.paint(p, ch, TextAlign::Left, None, window, cx);
                c = if cell.wide { c + 2 } else { e };
            }
        }
        // Выделение (S6-07): поверх текста, полупрозрачно.
        let sel = hsla(snap.cursor_color).opacity(0.35);
        for &(r, a, e) in &snap.selection {
            #[allow(clippy::cast_precision_loss, reason = "ширина в ячейках")]
            let w = cw * f32::from(e.saturating_sub(a) + 1);
            window.paint_quad(gpui_kit::fill(
                Bounds::new(at(usize::from(r), usize::from(a)), size2(w, ch)),
                sel,
            ));
        }
        // Курсор.
        let (cr, cc) = (usize::from(snap.cursor.0), usize::from(snap.cursor.1));
        let color = hsla(snap.cursor_color);
        let p = at(cr, cc);
        match snap.cursor_kind {
            CursorKind::Hidden => {}
            CursorKind::Beam => {
                window.paint_quad(gpui_kit::fill(Bounds::new(p, size2(px(2.0), ch)), color));
            }
            CursorKind::Underline => {
                window.paint_quad(gpui_kit::fill(
                    Bounds::new(point(p.x, p.y + ch - px(2.0)), size2(cw, px(2.0))),
                    color,
                ));
            }
            CursorKind::Block => {
                window.paint_quad(gpui_kit::fill(Bounds::new(p, size2(cw, ch)), color));
                if let Some(cell) = snap.cells.get(cr).and_then(|l| l.get(cc))
                    && cell.ch != ' '
                    && !procedural(cell.ch)
                {
                    let run = TextRun {
                        len: cell.ch.len_utf8(),
                        font: font.clone(),
                        color: hsla(snap.bg),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    };
                    let shaped = window.text_system().shape_line(
                        SharedString::from(cell.ch.to_string()),
                        px(text_px),
                        &[run],
                        None,
                    );
                    let _ = shaped.paint(p, ch, TextAlign::Left, None, window, cx);
                }
            }
        }
    });
    crate::bench::GRID_NS.fetch_add(
        u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// Процедурный глиф: линии рамок (тонкие, толстые, двойные) и блоки по ячейке.
#[allow(clippy::many_single_char_names, reason = "геометрия ячейки")]
fn box_glyph(
    c: char,
    o: gpui_kit::Point<Pixels>,
    w: Pixels,
    h: Pixels,
    color: Hsla,
    window: &mut Window,
) {
    let rect = |x: Pixels, y: Pixels, rw: Pixels, rh: Pixels, window: &mut Window| {
        window.paint_quad(gpui_kit::fill(
            Bounds::new(point(o.x + x, o.y + y), size2(rw, rh)),
            color,
        ));
    };
    let code = c as u32;
    // Стрелки Powerline U+E0B0–E0B3: сплошные и контурные треугольники во всю ячейку.
    if (0xE0B0..=0xE0B3).contains(&code) {
        let p = |x: Pixels, y: Pixels| point(o.x + x, o.y + y);
        let right = code <= 0xE0B1;
        let (tip, base) = if right { (w, px(0.0)) } else { (px(0.0), w) };
        let mut pb = if code == 0xE0B1 || code == 0xE0B3 {
            PathBuilder::stroke(px(1.0))
        } else {
            PathBuilder::fill()
        };
        pb.move_to(p(base, px(0.0)));
        pb.line_to(p(tip, h / 2.0));
        pb.line_to(p(base, h));
        if code == 0xE0B0 || code == 0xE0B2 {
            pb.close();
        }
        if let Ok(path) = pb.build() {
            window.paint_path(path, color);
        }
        return;
    }
    // Блоки U+2580–259F.
    if (0x2580..=0x259F).contains(&code) {
        let eighth = |n: f32| h * (n / 8.0);
        match code {
            0x2580 => rect(px(0.0), px(0.0), w, h / 2.0, window),
            0x2581..=0x2587 => {
                #[allow(clippy::cast_precision_loss, reason = "восьмые")]
                let n = (code - 0x2580) as f32;
                rect(px(0.0), h - eighth(n), w, eighth(n), window);
            }
            0x258C => rect(px(0.0), px(0.0), w / 2.0, h, window),
            0x2590 => rect(w / 2.0, px(0.0), w / 2.0, h, window),
            0x2591..=0x2593 => {
                let a = match code {
                    0x2591 => 0.25,
                    0x2592 => 0.5,
                    _ => 0.75,
                };
                window.paint_quad(gpui_kit::fill(
                    Bounds::new(o, size2(w, h)),
                    color.opacity(a),
                ));
            }
            0x2594 => rect(px(0.0), px(0.0), w, eighth(1.0), window),
            0x2595 => rect(w - w / 8.0, px(0.0), w / 8.0, h, window),
            // █ и прочие (квадранты) — вся ячейка.
            _ => rect(px(0.0), px(0.0), w, h, window),
        }
        return;
    }
    // Линии U+2500–257F: (влево, вправо, вверх, вниз) — 0 нет, 1 тонкая, 2 толстая, 3 двойная.
    let (l, r, u, d) = lines(code);
    let thin = px(1.0);
    let thick = px(2.0);
    let (cx, cy) = (w / 2.0, h / 2.0);
    let stroke = |kind: u8| if kind == 2 { thick } else { thin };
    let horiz = |from: Pixels, to: Pixels, kind: u8, window: &mut Window| {
        if kind == 3 {
            rect(from, cy - px(2.0), to - from, thin, window);
            rect(from, cy + px(1.0), to - from, thin, window);
        } else if kind > 0 {
            let t = stroke(kind);
            rect(from, cy - t / 2.0, to - from, t, window);
        }
    };
    let vert = |from: Pixels, to: Pixels, kind: u8, window: &mut Window| {
        if kind == 3 {
            rect(cx - px(2.0), from, thin, to - from, window);
            rect(cx + px(1.0), from, thin, to - from, window);
        } else if kind > 0 {
            let t = stroke(kind);
            rect(cx - t / 2.0, from, t, to - from, window);
        }
    };
    let pad = px(2.0);
    horiz(px(0.0), cx + if r > 0 { pad } else { px(1.0) }, l, window);
    horiz(cx - if l > 0 { pad } else { px(1.0) }, w, r, window);
    vert(px(0.0), cy + if d > 0 { pad } else { px(1.0) }, u, window);
    vert(cy - if u > 0 { pad } else { px(1.0) }, h, d, window);
}

/// Стороны линии рамки по коду (основные: ─ │ ┌ ┐ └ ┘ ├ ┤ ┬ ┴ ┼, толстые и двойные).
fn lines(code: u32) -> (u8, u8, u8, u8) {
    match code {
        0x2500 => (1, 1, 0, 0),
        0x2501 => (2, 2, 0, 0),
        0x2502 => (0, 0, 1, 1),
        0x2503 => (0, 0, 2, 2),
        0x250C | 0x256D => (0, 1, 0, 1),
        0x250F => (0, 2, 0, 2),
        0x2510 | 0x256E => (1, 0, 0, 1),
        0x2513 => (2, 0, 0, 2),
        0x2514 | 0x2570 => (0, 1, 1, 0),
        0x2517 => (0, 2, 2, 0),
        0x2518 | 0x256F => (1, 0, 1, 0),
        0x251B => (2, 0, 2, 0),
        0x251C => (0, 1, 1, 1),
        0x2523 => (0, 2, 2, 2),
        0x2524 => (1, 0, 1, 1),
        0x252B => (2, 0, 2, 2),
        0x252C => (1, 1, 0, 1),
        0x2533 => (2, 2, 0, 2),
        0x2534 => (1, 1, 1, 0),
        0x253B => (2, 2, 2, 0),
        0x254B => (2, 2, 2, 2),
        0x2550 => (3, 3, 0, 0),
        0x2551 => (0, 0, 3, 3),
        0x2552 => (0, 3, 0, 1),
        0x2553 => (0, 1, 0, 3),
        0x2554 => (0, 3, 0, 3),
        0x2555 => (3, 0, 0, 1),
        0x2556 => (1, 0, 0, 3),
        0x2557 => (3, 0, 0, 3),
        0x2558 => (0, 3, 1, 0),
        0x2559 => (0, 1, 3, 0),
        0x255A => (0, 3, 3, 0),
        0x255B => (3, 0, 1, 0),
        0x255C => (1, 0, 3, 0),
        0x255D => (3, 0, 3, 0),
        0x255E => (0, 3, 1, 1),
        0x255F => (0, 1, 3, 3),
        0x2560 => (0, 3, 3, 3),
        0x2561 => (3, 0, 1, 1),
        0x2562 => (1, 0, 3, 3),
        0x2563 => (3, 0, 3, 3),
        0x2564 => (3, 3, 0, 1),
        0x2565 => (1, 1, 0, 3),
        0x2566 => (3, 3, 0, 3),
        0x2567 => (3, 3, 1, 0),
        0x2568 => (1, 1, 3, 0),
        0x2569 => (3, 3, 3, 0),
        0x256A => (3, 3, 1, 1),
        0x256B => (1, 1, 3, 3),
        0x256C => (3, 3, 3, 3),
        0x2574 => (1, 0, 0, 0),
        0x2575 => (0, 0, 1, 0),
        0x2576 => (0, 1, 0, 0),
        0x2577 => (0, 0, 0, 1),
        // ┼ и прочие (смешанные толщины, штрихи) — тонкий крест.
        _ => (1, 1, 1, 1),
    }
}

/// Терминал как фича окна (ADR-0022): сетка вместо спрятанных панелей, мышь и колесо
/// над ней, размер PTY по окну, просьбы терминала к окну (панели, буфер обмена).
#[derive(Default)]
pub(super) struct Term {
    /// Снапшот сетки (S6-06) и чей он (адрес `TermShared`, тёмная тема):
    /// пересобирается, когда сеанс поднял флаг изменений или сменился.
    snap: Option<((usize, bool), Rc<Snapshot>)>,
    /// Последний отправленный размер области терминала, ячеек (S6-08).
    area: (u16, u16),
    /// Где нарисована сетка (мышь → ячейка, S6-07) и сдвиг строк вида transcript.
    bounds: GridBounds,
    shift: isize,
    /// Колесо: дробная часть строк; последняя ячейка протяжки.
    wheel: f32,
    last: Option<(i64, i64, bool)>,
}

impl Term {
    /// Снапшот видимого терминала: показанный явно (`TermState.preview`, сцены), иначе
    /// активного сеанса. Пересобирается только по флагу изменений или смене сеанса.
    fn snapshot(&mut self, dark: bool, s: &AppState) -> Option<Rc<Snapshot>> {
        let st = s.ext.get::<urfin_term::TermState>()?;
        let shared = st
            .preview
            .as_ref()
            .or_else(|| st.sessions.active().and_then(|a| a.shared()));
        let Some(shared) = shared else {
            self.snap = None;
            return None;
        };
        let key = (std::sync::Arc::as_ptr(shared) as usize, dark);
        let dirty = shared.take_dirty();
        match &self.snap {
            Some((k, snap)) if *k == key && !dirty => Some(snap.clone()),
            _ => {
                let t = std::time::Instant::now();
                let snap = Rc::new(shared.snapshot(&defaults(dark)));
                crate::bench::GRID_NS.fetch_add(
                    u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    std::sync::atomic::Ordering::Relaxed,
                );
                self.snap = Some((key, snap.clone()));
                Some(snap)
            }
        }
    }

    /// Мышь над сеткой → `term.mouse` в ячейках экрана (S6-07). Протяжка — только при
    /// смене ячейки.
    fn mouse(
        &mut self,
        kind: &'static str,
        pos: gpui_kit::Point<Pixels>,
        m: gpui_kit::Modifiers,
        clicks: usize,
        text_px: f32,
        window: &mut Window,
    ) -> Option<Command> {
        let b = self.bounds.get()?;
        let (row, col, right) = cell_at(b, pos, metrics(text_px, window));
        let row = (row + i64::try_from(self.shift).unwrap_or(0)).max(0);
        if kind == "move" && self.last == Some((row, col, right)) {
            return None;
        }
        self.last = Some((row, col, right));
        let mut c = Command::new("term.mouse")
            .arg("kind", kind)
            .arg("button", "left")
            .arg("row", row)
            .arg("col", col)
            .arg("right", right)
            .arg("clicks", i64::try_from(clicks).unwrap_or(1));
        let mods: Vec<&str> = [(m.control, "ctrl"), (m.alt, "alt"), (m.shift, "shift")]
            .into_iter()
            .filter_map(|(on, n)| on.then_some(n))
            .collect();
        if !mods.is_empty() {
            c = c.arg("mods", mods.join("+"));
        }
        Some(c)
    }

    /// Колесо над сеткой: строки с дробной частью; `+` — вверх, к истории.
    fn scroll(
        &mut self,
        ev: &gpui_kit::ScrollWheelEvent,
        text_px: f32,
        window: &mut Window,
    ) -> Option<Command> {
        let cell = metrics(text_px, window);
        let dy = f32::from(ev.delta.pixel_delta(cell.1).y);
        self.wheel += dy / f32::from(cell.1);
        let whole = self.wheel.trunc();
        self.wheel -= whole;
        #[allow(clippy::cast_possible_truncation, reason = "строк за событие немного")]
        let lines = whole as i64;
        if lines == 0 {
            return None;
        }
        let (row, col, _) = cell_at(self.bounds.get()?, ev.position, cell);
        let mut c = Command::new("term.mouse")
            .arg("kind", "wheel")
            .arg("row", row)
            .arg("col", col)
            .arg("lines", lines);
        if ev.modifiers.shift {
            c = c.arg("mods", "shift");
        }
        Some(c)
    }
}

/// Команда мыши фичи терминала — через хост (как клик).
fn route(
    h: &mut CoreHost,
    w: &mut Window,
    cx: &mut Context<CoreHost>,
    f: impl FnOnce(&mut Term, f32, &mut Window) -> Option<Command>,
) {
    let text_px = h.theme.m(Metric::TypeMono);
    if let Some(c) = h.features.get_mut::<Term>().and_then(|t| f(t, text_px, w)) {
        h.run(c, w, cx);
    }
}

impl ShellFeature for Term {
    /// Просьбы терминала к окну: `term.panels` — спрятать или вернуть панели (автопоказ
    /// S6-08, `term.toggle`); `clip.write` — строку в буфер обмена, `clip.read` — текст
    /// буфера в команду `then` (S6-15).
    fn ext(
        &mut self,
        id: &str,
        args: &Args,
        s: &AppState,
        cx: &mut Context<CoreHost>,
    ) -> Option<Vec<Command>> {
        match id {
            "term.panels" => {
                let hidden = s
                    .ext
                    .get::<crate::UiState>()
                    .is_some_and(|u| u.panels_hidden);
                let mut c = Command::new("ui.panels.toggle_all");
                if let Some(show) = args.bool("show") {
                    c = c.arg("show", show);
                } else if args.bool("refocus").is_some() {
                    // Вид терминала сменился: то же состояние панелей, фокус — заново.
                    c = c.arg("show", !hidden);
                }
                Some(vec![c])
            }
            "clip.write" => {
                let text = args.str("text").unwrap_or_default().to_owned();
                cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text));
                Some(Vec::new())
            }
            "clip.read" => {
                let then = match args.str("then") {
                    Some("cmdline.paste") => "cmdline.paste",
                    Some("term.paste") => "term.paste",
                    _ => return Some(Vec::new()),
                };
                let text = cx.read_from_clipboard().and_then(|c| c.text());
                Some(
                    text.map(|t| Command::new(then).arg("text", t))
                        .into_iter()
                        .collect(),
                )
            }
            _ => None,
        }
    }

    /// Область терминала (S6 §3.1) — вся площадь между заголовком и статус-строкой,
    /// с рядом cmdline: от видимости панелей и cmdline не зависит. Размер в ячейках —
    /// в Core (`term.resize`, дебаунс там).
    fn sync(&mut self, theme: &Theme, window: &mut Window) -> Option<Command> {
        let size = window.viewport_size();
        let (cw, ch) = metrics(theme.m(Metric::TypeMono), window);
        let w = f32::from(size.width) - 2.0 * PAD_X;
        let h = f32::from(size.height)
            - theme.m(Metric::HTitlebar)
            - theme.m(Metric::HStatus)
            - theme.m(Metric::HKeybar)
            - 2.0 * PAD_Y;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "ячеек в окне — до тысяч"
        )]
        let cells = |len: f32, cell: Pixels| (len / f32::from(cell)).floor().max(2.0) as u16;
        let area = (cells(w, cw), cells(h, ch));
        if area == self.area {
            return None;
        }
        self.area = area;
        Some(
            Command::new("term.resize")
                .arg("cols", i64::from(area.0))
                .arg("rows", i64::from(area.1)),
        )
    }

    /// Панели спрятаны (Ctrl+O) — за ними сетка терминала (S6-06); с интеграцией, у
    /// промпта — transcript над cmdline (S6 §3.2).
    fn main(
        &mut self,
        f: &Frame<'_>,
        hidden: bool,
        window: &mut Window,
        cx: &Context<CoreHost>,
    ) -> Option<Main> {
        self.shift = 0;
        if !hidden {
            self.snap = None;
            return None;
        }
        let (s, theme) = (f.s, f.theme);
        let snap = self.snapshot(theme.tokens.dark(), s)?;
        let row = s
            .ext
            .get::<urfin_term::TermState>()
            .filter(|t| !t.screen_view())
            .and_then(urfin_term::TermState::prompt_row);
        let snap = match row {
            Some(row) => {
                let (_, ch) = metrics(theme.m(Metric::TypeMono), window);
                let h = f32::from(window.viewport_size().height)
                    - theme.m(Metric::HTitlebar)
                    - theme.m(Metric::HCmdline)
                    - theme.m(Metric::HStatus)
                    - theme.m(Metric::HKeybar)
                    - 2.0 * PAD_Y;
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "строк в окне"
                )]
                let fit = (h / f32::from(ch)).floor().max(1.0) as usize;
                self.shift = transcript_shift(snap.cells.len(), row, fit);
                Rc::new(transcript(&snap, row, fit))
            }
            None => snap,
        };
        Some(Main::Cover(
            gpui_kit::div()
                .id("term-grid")
                .absolute()
                .inset_0()
                .child(view(snap, theme.m(Metric::TypeMono), self.bounds.clone()))
                // Мышь (S6-07): выделение, отчёты приложению, колесо.
                .on_mouse_down(
                    gpui_kit::MouseButton::Left,
                    cx.listener(|h, ev: &gpui_kit::MouseDownEvent, w, cx| {
                        route(h, w, cx, |t, px, w| {
                            t.mouse("down", ev.position, ev.modifiers, ev.click_count, px, w)
                        });
                    }),
                )
                .on_mouse_up(
                    gpui_kit::MouseButton::Left,
                    cx.listener(|h, ev: &gpui_kit::MouseUpEvent, w, cx| {
                        route(h, w, cx, |t, px, w| {
                            t.mouse("up", ev.position, ev.modifiers, ev.click_count, px, w)
                        });
                    }),
                )
                .on_mouse_move(cx.listener(|h, ev: &gpui_kit::MouseMoveEvent, w, cx| {
                    if ev.pressed_button == Some(gpui_kit::MouseButton::Left) {
                        route(h, w, cx, |t, px, w| {
                            t.mouse("move", ev.position, ev.modifiers, 1, px, w)
                        });
                    }
                }))
                .on_scroll_wheel(cx.listener(|h, ev: &gpui_kit::ScrollWheelEvent, w, cx| {
                    route(h, w, cx, |t, px, w| t.scroll(ev, px, w));
                }))
                .into_any_element(),
        ))
    }
}

/// Сцены сетки терминала за спрятанными панелями (S6-06): экраны демо-командой.
pub(super) const SCENES: &[Scene] = &[
    Scene {
        name: "term-frames",
        steps: &["ui.cancel", "screens.term kind=frames"],
    },
    Scene {
        name: "term-unicode",
        steps: &["screens.term kind=unicode"],
    },
    Scene {
        name: "term-colors",
        steps: &["screens.term kind=colors"],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_keeps_selection_rows() {
        let snap = Snapshot {
            cols: 4,
            rows: 6,
            cells: vec![Vec::new(); 6],
            cursor: (0, 0),
            cursor_kind: CursorKind::Block,
            cursor_color: [0; 3],
            bg: [0; 3],
            selection: vec![(1, 0, 3), (2, 1, 2), (5, 0, 0)],
        };
        // Промпт в строке 3, вид на 4 строки: строки 0..3 прижаты к низу.
        let t = transcript(&snap, 3, 4);
        assert_eq!(t.selection, [(2, 0, 3), (3, 1, 2)]);
        assert_eq!(transcript_shift(6, 3, 4), -1);
        // Строка вида 2 → строка экрана 1.
        assert_eq!(2 + transcript_shift(6, 3, 4), 1);
    }

    #[test]
    fn box_sides() {
        assert_eq!(lines(0x2554), (0, 3, 0, 3));
        assert_eq!(lines(0x2500), (1, 1, 0, 0));
        assert!(procedural('═') && procedural('█') && procedural('\u{E0B0}'));
        assert!(!procedural('A') && !procedural('漢'));
    }
}
