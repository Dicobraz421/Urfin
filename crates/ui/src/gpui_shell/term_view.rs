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
    App, Bounds, Font, Hsla, PathBuilder, Pixels, SharedString, TextAlign, TextRun, Window, canvas,
    point, px, size as size2,
};
use urfin_term::vt::grid::{Cell, CursorKind, Snapshot};

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
