//! Вывод без VT (F03 §3.6, режим «Текст» и поиск): управляющие последовательности
//! убираются, остаётся то, что программа напечатала. Состояние — между событиями:
//! последовательность может попасть на границу двух событий записи.

/// Где разбор.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum St {
    #[default]
    Text,
    Esc,
    /// `ESC [` … финальный байт.
    Csi,
    /// `ESC ]`, `ESC P`, `ESC X`, `ESC ^`, `ESC _` … BEL или ST.
    Str,
    /// `ESC` внутри строки: дальше `\`.
    StrEsc,
    /// `ESC (` и подобные: ещё один байт.
    Skip,
}

/// Убирает VT из потока вывода.
#[derive(Clone, Copy, Debug, Default)]
pub struct Strip {
    st: St,
    /// `\r` ждёт следующего символа: `\r\n` — один перевод строки, одиночный `\r` — тоже
    /// новая строка (прогресс и перерисовка строки не склеиваются в одну).
    cr: bool,
}

impl Strip {
    /// Дописывает текст `s` без VT в `out`.
    pub fn feed(&mut self, s: &str, out: &mut Vec<u8>) {
        for &b in s.as_bytes() {
            match self.st {
                St::Text => self.text(b, out),
                St::Esc => {
                    self.st = match b {
                        b'[' => St::Csi,
                        b']' | b'P' | b'X' | b'^' | b'_' => St::Str,
                        b'(' | b')' | b'*' | b'+' | b'-' | b'.' | b'/' | b'#' | b'%' | b' ' => {
                            St::Skip
                        }
                        0x1b => St::Esc,
                        _ => St::Text,
                    };
                }
                St::Csi => match b {
                    0x40..=0x7e => self.st = St::Text,
                    0x1b => self.st = St::Esc,
                    _ => {}
                },
                St::Str => match b {
                    0x07 => self.st = St::Text,
                    0x1b => self.st = St::StrEsc,
                    _ => {}
                },
                St::StrEsc | St::Skip => self.st = St::Text,
            }
        }
    }

    fn text(&mut self, b: u8, out: &mut Vec<u8>) {
        match b {
            0x1b => self.st = St::Esc,
            b'\r' => self.cr = true,
            b'\n' => {
                self.cr = false;
                out.push(b'\n');
            }
            0x08 => {
                // Забой: последний символ строки, если он ещё в этом куске.
                while let Some(&last) = out.last() {
                    if last == b'\n' {
                        break;
                    }
                    out.pop();
                    if last & 0xC0 != 0x80 {
                        break;
                    }
                }
            }
            b'\t' | 0x20..=0x7e | 0x80..=0xff => {
                if std::mem::take(&mut self.cr) {
                    out.push(b'\n');
                }
                out.push(b);
            }
            _ => {}
        }
    }
}
