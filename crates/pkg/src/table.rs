//! Таблицы winget (F02 §2.3): строка из `-` отмечает начало данных, границы колонок —
//! начала слов заголовка над ней. Позиции — в ячейках экрана (широкий символ — две):
//! winget выравнивает по ширине, не по символам. Текст заголовков не читается — он на
//! языке Windows; роли колонок определяет `winget.rs` по содержимому.
//!
//! Строка, в которой перед границей колонки стоит не пробел, в сетку не попадает
//! (хвостовые «N upgrades available», пины, сообщения). Короткие хвосты сетку не
//! ломают: их отбрасывает вызывающий по пустым обязательным ячейкам.

use unicode_width::UnicodeWidthChar;

/// Короче — не разделитель (крутилка прогресса `-`, тире в тексте).
const MIN_SEPARATOR: usize = 10;

/// Таблица под одной строкой-разделителем.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    /// Начала колонок, ячейки экрана.
    pub starts: Vec<usize>,
    /// Строки, попавшие в сетку: ячейка на колонку, без пробелов по краям.
    pub rows: Vec<Vec<String>>,
}

fn is_separator(line: &str) -> bool {
    let t = line.trim_end();
    t.len() >= MIN_SEPARATOR && t.bytes().all(|b| b == b'-')
}

fn width(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// Начала слов строки, ячейки экрана.
fn word_starts(line: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut col = 0;
    let mut prev_space = true;
    for c in line.chars() {
        let space = c == ' ';
        if !space && prev_space {
            out.push(col);
        }
        prev_space = space;
        col += width(c);
    }
    out
}

/// Ячейки строки по началам колонок; `None` — строка не в сетке: символ перед
/// границей внутри строки — не пробел, или широкий символ её пересекает.
pub fn cells(line: &str, starts: &[usize]) -> Option<Vec<String>> {
    let mut cells = vec![String::new(); starts.len()];
    let mut col = 0;
    let mut idx = 0;
    let mut prev_space = true;
    for c in line.chars() {
        let w = width(c);
        while idx + 1 < starts.len() && col >= starts[idx + 1] {
            idx += 1;
            if !prev_space {
                return None;
            }
        }
        if starts
            .get(idx + 1)
            .is_some_and(|&next| col < next && col + w > next)
        {
            return None;
        }
        cells[idx].push(c);
        prev_space = c == ' ';
        col += w;
    }
    for cell in &mut cells {
        let t = cell.trim();
        if t.len() != cell.len() {
            *cell = t.to_owned();
        }
    }
    Some(cells)
}

/// Все таблицы вывода по порядку. Строка над разделителем — заголовок, не данные;
/// таблица кончается у заголовка следующей.
pub fn parse(lines: &[String]) -> Vec<Table> {
    let seps: Vec<usize> = (1..lines.len())
        .filter(|&i| is_separator(&lines[i]))
        .collect();
    let mut out = Vec::with_capacity(seps.len());
    for (k, &sep) in seps.iter().enumerate() {
        let starts = word_starts(&lines[sep - 1]);
        if starts.is_empty() {
            continue;
        }
        let end = seps.get(k + 1).map_or(lines.len(), |&next| next - 1);
        let rows = lines[sep + 1..end.max(sep + 1)]
            .iter()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| cells(l, &starts))
            .collect();
        out.push(Table { starts, rows });
    }
    out
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    #[test]
    fn grid_by_header_words() {
        let t = parse(&lines(
            "junk\n\
             Name      Id        Version\n\
             ---------------------------\n\
             Foo Bar   Foo.Bar   1.0\n\
             Wide 漢字 W.X       2\n\
             3 upgrades available.\n",
        ));
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].starts, [0, 10, 20]);
        assert_eq!(
            t[0].rows,
            [
                vec!["Foo Bar", "Foo.Bar", "1.0"],
                vec!["Wide 漢字", "W.X", "2"],
            ]
        );
    }

    #[test]
    fn off_grid_lines_dropped() {
        let starts = [0, 5, 10];
        assert_eq!(cells("abcdefghijkl", &starts), None);
        assert_eq!(cells("abc  d fgh x", &starts), None);
        // Широкий символ на границе.
        assert_eq!(cells("abc 漢x", &starts), None);
        assert_eq!(
            cells("abc  de   x", &starts),
            Some(vec!["abc".into(), "de".into(), "x".into()])
        );
        assert_eq!(cells("", &starts), Some(vec![String::new(); 3]));
    }

    #[test]
    fn several_tables() {
        let t = parse(&lines(
            "A  B\n\
             ----------\n\
             a1 b1\n\
             \n\
             text between tables\n\
             C   D\n\
             ----------\n\
             c1  d1\n",
        ));
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].rows, [vec!["a1", "b1"]]);
        assert_eq!(t[1].rows, [vec!["c1", "d1"]]);
    }

    #[test]
    fn no_table() {
        assert_eq!(parse(&lines("No installed package found.\n---\n")), []);
        assert_eq!(parse(&lines("----------\nA B\n")), []);
    }

    /// Ячейка шириной `w` как у winget: длиннее — усечение с `…`.
    fn fit(s: &str, w: usize) -> String {
        let mut out = String::new();
        let mut used = 0;
        let total: usize = s.chars().map(width).sum();
        for c in s.chars() {
            let cw = width(c);
            if total > w && used + cw + 1 > w {
                out.push('…');
                break;
            }
            if used + cw > w {
                break;
            }
            out.push(c);
            used += cw;
        }
        out
    }

    fn pad(s: &str, w: usize) -> String {
        let used: usize = s.chars().map(width).sum();
        format!("{s}{}", " ".repeat(w.saturating_sub(used)))
    }

    proptest! {
        /// Произвольные ширины колонок и значения (в имени — пробелы и широкие
        /// символы): разбор возвращает те же ячейки, усечённые как у winget.
        #[test]
        fn roundtrip(
            widths in proptest::collection::vec(2usize..24, 2..6),
            rows in proptest::collection::vec(
                proptest::collection::vec("[a-zA-Z0-9.+]{1,30}( [a-z漢字]{1,6}){0,2}", 6),
                0..8,
            ),
        ) {
            let header: Vec<String> = (0..widths.len()).map(|i| format!("H{i}")).collect();
            let widths: Vec<usize> = widths
                .iter()
                .zip(&header)
                .map(|(w, h)| (*w).max(h.len()))
                .collect();
            let render = |cells: &[String]| {
                let last = cells.len() - 1;
                cells
                    .iter()
                    .enumerate()
                    .map(|(i, c)| if i == last { c.clone() } else { pad(c, widths[i]) })
                    .collect::<Vec<_>>()
                    .join(" ")
                    .trim_end()
                    .to_owned()
            };
            let total: usize = widths.iter().sum::<usize>() + widths.len() - 1;
            let mut text = vec![render(&header), "-".repeat(total.max(MIN_SEPARATOR))];
            let mut want = Vec::new();
            for row in &rows {
                let cells: Vec<String> = widths
                    .iter()
                    .zip(row)
                    .enumerate()
                    .map(|(i, (w, v))| {
                        let v = if i == 0 { v.as_str() } else { v.split(' ').next().unwrap() };
                        if i == widths.len() - 1 { v.to_owned() } else { fit(v, *w) }
                    })
                    .collect();
                text.push(render(&cells));
                want.push(cells.iter().map(|c| c.trim().to_owned()).collect::<Vec<_>>());
            }
            let t = parse(&text);
            prop_assert_eq!(t.len(), 1);
            let mut starts = vec![0];
            for w in &widths[..widths.len() - 1] {
                starts.push(starts.last().unwrap() + w + 1);
            }
            prop_assert_eq!(&t[0].starts, &starts);
            prop_assert_eq!(&t[0].rows, &want);
        }
    }
}
