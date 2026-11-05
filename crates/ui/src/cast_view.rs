//! Просмотр записи сеанса без gpui (F03 §3.6, `screen-sshrecview`): статус одной группой
//! (№92) с поправкой UTC, совпадения поиска на экране плеера. Рисует
//! `gpui_shell::view::cast`.

use std::ops::Range;
use std::time::Duration;

use urfin_i18n::FluentArgs;
use urfin_term::vt::grid::Snapshot;
use urfin_view::cast::Header;
use urfin_view::search::Matcher;

use crate::sys::Zone;

/// Поправка пояса `z` в момент `unix_s`: «UTC+3», «UTC+5:30», «UTC-4», «UTC».
pub fn utc_label(z: &Zone, unix_s: i64) -> String {
    let ns = unix_s.saturating_mul(1_000_000_000);
    let (Some(l), Some(u)) = (z.local(ns), Zone::utc().local(ns)) else {
        return "UTC".to_owned();
    };
    let day = (l.year, l.month, l.day).cmp(&(u.year, u.month, u.day)) as i32;
    let min = |h: u8, m: u8| i32::from(h) * 60 + i32::from(m);
    let off = day * 1440 + min(l.hour, l.minute) - min(u.hour, u.minute);
    if off == 0 {
        return "UTC".to_owned();
    }
    let sign = if off < 0 { '-' } else { '+' };
    let (h, m) = (off.abs() / 60, off.abs() % 60);
    if m == 0 {
        format!("UTC{sign}{h}")
    } else {
        format!("UTC{sign}{h}:{m:02}")
    }
}

/// «23.10.26 14:02 UTC+3».
pub fn start_text(z: &Zone, unix_s: i64) -> Option<String> {
    let l = z.local(unix_s.saturating_mul(1_000_000_000))?;
    Some(format!(
        "{:02}.{:02}.{:02} {:02}:{:02} {}",
        l.day,
        l.month,
        l.year.rem_euclid(100),
        l.hour,
        l.minute,
        utc_label(z, unix_s),
    ))
}

/// Статус одной группой: «Сеанс admin@srv1 · терминал 132×30 · начало 23.10.26 14:02
/// UTC+3 · длительность 12:47 · asciicast v2, 1,2 МБ». Нет названия или начала в
/// заголовке (чужая запись) — без этих частей.
pub fn status(h: &Header, duration_ms: u64, size: u64, z: &Zone) -> String {
    let i18n = urfin_i18n::current();
    let mut parts = Vec::with_capacity(5);
    let one = |key: &str, name: &str, value: String| {
        let mut a = FluentArgs::new();
        a.set(name.to_owned(), value);
        i18n.fmt(key, &a)
    };
    if !h.title.is_empty() {
        parts.push(one("ui-cast-session", "title", h.title.clone()));
    }
    parts.push(one(
        "ui-cast-term",
        "size",
        format!("{}×{}", h.width, h.height),
    ));
    let start = h
        .timestamp
        .and_then(|t| i64::try_from(t).ok())
        .and_then(|t| start_text(z, t));
    if let Some(t) = start {
        parts.push(one("ui-cast-start", "time", t));
    }
    parts.push(one(
        "ui-cast-duration",
        "time",
        crate::term_rec::clock(Duration::from_millis(duration_ms)),
    ));
    parts.push(one(
        "ui-cast-format",
        "size",
        urfin_i18n::fmt::bytes(size),
    ));
    parts.join(" · ")
}

/// Совпадение на экране: строка, колонки.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub row: usize,
    pub cols: Range<usize>,
}

/// Совпадения образца на экране, сверху вниз.
pub fn hits(snap: &Snapshot, m: &Matcher) -> Vec<Hit> {
    let mut out = Vec::new();
    let mut text = String::new();
    // Колонка каждого байта текста.
    let mut cols: Vec<usize> = Vec::new();
    for (row, cells) in snap.cells.iter().enumerate() {
        text.clear();
        cols.clear();
        for (c, cell) in cells.iter().enumerate().filter(|(_, x)| !x.spacer) {
            text.push(cell.ch);
            cols.resize(text.len(), c);
        }
        let p = m.prepare(text.as_bytes());
        let mut at = 0;
        while let Some(r) = p.find_at(at) {
            if r.is_empty() {
                break;
            }
            let last = cols[r.end - 1];
            let width = usize::from(cells[last].wide) + 1;
            out.push(Hit {
                row,
                cols: cols[r.start]..last + width,
            });
            at = r.end;
        }
    }
    out
}

/// Цвета совпадения: фон и текст.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Paint {
    pub bg: [u8; 3],
    pub fg: [u8; 3],
}

/// Красит совпадения; `cur` — номер текущего среди них.
pub fn paint(snap: &mut Snapshot, list: &[Hit], cur: Option<usize>, hit: Paint, now: Paint) {
    for (i, h) in list.iter().enumerate() {
        let p = if Some(i) == cur { now } else { hit };
        for c in &mut snap.cells[h.row][h.cols.clone()] {
            c.bg = p.bg;
            c.fg = p.fg;
            c.plain = false;
        }
    }
}

/// Какое из `visible` совпадений экрана — текущее `n` (с 1), если к этому моменту
/// выведено `shown` совпадений: на экране — последние из выведенных.
pub fn current(visible: usize, shown: usize, n: usize) -> Option<usize> {
    let back = shown.checked_sub(n)?;
    visible.checked_sub(1)?.checked_sub(back)
}

#[cfg(test)]
mod tests {
    use super::*;
    use urfin_view::Enc;
    use urfin_view::search::{Pattern, compile};

    fn snap(lines: &[&str]) -> Snapshot {
        let cell = |ch| urfin_term::vt::grid::Cell {
            ch,
            fg: [0; 3],
            bg: [255; 3],
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            wide: false,
            spacer: false,
            plain: true,
        };
        Snapshot {
            cols: 20,
            rows: 2,
            cells: lines
                .iter()
                .map(|l| {
                    let mut row = Vec::new();
                    for ch in l.chars() {
                        let wide = ch == '漢';
                        row.push(urfin_term::vt::grid::Cell { wide, ..cell(ch) });
                        if wide {
                            row.push(urfin_term::vt::grid::Cell {
                                spacer: true,
                                ..cell(' ')
                            });
                        }
                    }
                    row
                })
                .collect(),
            cursor: (0, 0),
            cursor_kind: urfin_term::vt::grid::CursorKind::Block,
            cursor_color: [0; 3],
            bg: [255; 3],
            selection: Vec::new(),
            alt: false,
        }
    }

    #[test]
    fn hits_in_columns() {
        let m = compile(
            &Pattern::Text {
                s: "failed".into(),
                case: false,
                word: false,
            },
            Enc::utf8(),
        )
        .unwrap();
        let s = snap(&["漢 Failed x failed", "ok"]);
        let list = hits(&s, &m);
        assert_eq!(
            list,
            [
                Hit {
                    row: 0,
                    cols: 3..9
                },
                Hit {
                    row: 0,
                    cols: 12..18
                }
            ]
        );
        let mut p = s.clone();
        let (a, b) = (
            Paint {
                bg: [1; 3],
                fg: [2; 3],
            },
            Paint {
                bg: [3; 3],
                fg: [4; 3],
            },
        );
        paint(&mut p, &list, Some(1), a, b);
        assert_eq!(p.cells[0][3].bg, [1; 3]);
        assert_eq!(p.cells[0][12].bg, [3; 3]);
        assert_eq!(p.cells[0][2].bg, [255; 3]);
        assert!(!p.cells[0][12].plain);
    }

    #[test]
    fn current_among_visible() {
        // 4 выведено, все на экране; текущее второе.
        assert_eq!(current(4, 4, 2), Some(1));
        // Выведено 10, на экране 3 последних; 9-е — среднее.
        assert_eq!(current(3, 10, 9), Some(1));
        // Текущее ушло с экрана или ещё не выведено.
        assert_eq!(current(3, 10, 2), None);
        assert_eq!(current(3, 2, 5), None);
    }

    #[test]
    fn status_one_group() {
        urfin_i18n::set_language("ru");
        let h = Header {
            width: 132,
            height: 30,
            timestamp: Some(1_792_753_320),
            title: "admin@srv1".into(),
            body: 0,
        };
        let st = status(&h, 767_000, 1_258_291, &Zone::utc());
        assert_eq!(
            st,
            "Сеанс admin@srv1 · терминал 132×30 · начало 23.10.26 11:02 UTC · длительность 12:47 · asciicast v2, 1,2 МБ"
        );
        let foreign = Header {
            timestamp: None,
            title: String::new(),
            ..h
        };
        assert_eq!(
            status(&foreign, 5_000, 2_048, &Zone::utc()),
            "терминал 132×30 · длительность 00:05 · asciicast v2, 2,0 КБ"
        );
    }
}
