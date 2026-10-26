//! Win32-input-mode (S6-20, S6 §3.5): `ConPTY` включает его `CSI ?9001h` в выводе,
//! и тогда клавиша уходит записью `INPUT_RECORD` целиком — `CSI Vk;Sc;Uc;Kd;Cs;Rc _`
//! (виртуальный код, скан-код, символ, нажата, состояние модификаторов, повтор).
//! Консольные Win32-программы (Far внутри терминала) получают то, что не выразить
//! в VT: Ctrl+Space, Alt+F7, Shift+F-клавиши, Ctrl+цифры.
//!
//! Клавиша приходит именем (как у [`super::input::key`]); виртуальный код и скан-код
//! (набор 1) — по таблице американской раскладки: для служебных клавиш раскладка не
//! важна, печатный текст (и `AltGr`) идёт текстом — его `ConPTY` превращает в записи
//! сам. Отпускание отправляется сразу за нажатием.

use super::input::Mods;

/// Флаги `dwControlKeyState`.
const LEFT_ALT: u32 = 0x02;
const LEFT_CTRL: u32 = 0x08;
const SHIFT: u32 = 0x10;
const ENHANCED: u32 = 0x100;

/// Клавиша: виртуальный код, скан-код, расширенная (серые стрелки, Ins/Del…).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vk {
    pub vk: u16,
    pub sc: u16,
    pub enhanced: bool,
}

#[allow(clippy::unnecessary_wraps, reason = "строка таблицы `lookup`")]
const fn vk(vk: u16, sc: u16) -> Option<Vk> {
    Some(Vk {
        vk,
        sc,
        enhanced: false,
    })
}

#[allow(clippy::unnecessary_wraps, reason = "строка таблицы `lookup`")]
const fn ext(vk: u16, sc: u16) -> Option<Vk> {
    Some(Vk {
        vk,
        sc,
        enhanced: true,
    })
}

/// Скан-коды набора 1 для букв `a`…`z`.
const LETTER_SC: [u16; 26] = [
    0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19,
    0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C,
];

/// Имя клавиши → коды; `None` — неизвестна.
pub fn lookup(name: &str) -> Option<Vk> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return char_key(c.to_ascii_lowercase());
    }
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<u16>().ok())
        && (1..=24).contains(&n)
    {
        let sc = match n {
            1..=10 => 0x3A + n,
            11 => 0x57,
            12 => 0x58,
            _ => 0x64 + (n - 13),
        };
        return vk(0x6F + n, sc);
    }
    match name {
        "enter" => vk(0x0D, 0x1C),
        "tab" => vk(0x09, 0x0F),
        "backspace" => vk(0x08, 0x0E),
        "escape" => vk(0x1B, 0x01),
        "space" => vk(0x20, 0x39),
        "up" => ext(0x26, 0x48),
        "down" => ext(0x28, 0x50),
        "left" => ext(0x25, 0x4B),
        "right" => ext(0x27, 0x4D),
        "home" => ext(0x24, 0x47),
        "end" => ext(0x23, 0x4F),
        "pageup" => ext(0x21, 0x49),
        "pagedown" => ext(0x22, 0x51),
        "insert" => ext(0x2D, 0x52),
        "delete" => ext(0x2E, 0x53),
        _ => None,
    }
}

/// Печатная клавиша американской раскладки (без Shift).
fn char_key(c: char) -> Option<Vk> {
    match c {
        'a'..='z' => {
            let i = c as u16 - u16::from(b'a');
            vk(0x41 + i, LETTER_SC[usize::from(i)])
        }
        '1'..='9' => vk(c as u16, 0x02 + (c as u16 - u16::from(b'1'))),
        '0' => vk(0x30, 0x0B),
        ' ' => vk(0x20, 0x39),
        '-' => vk(0xBD, 0x0C),
        '=' => vk(0xBB, 0x0D),
        '[' => vk(0xDB, 0x1A),
        ']' => vk(0xDD, 0x1B),
        ';' => vk(0xBA, 0x27),
        '\'' => vk(0xDE, 0x28),
        '`' => vk(0xC0, 0x29),
        '\\' => vk(0xDC, 0x2B),
        ',' => vk(0xBC, 0x33),
        '.' => vk(0xBE, 0x34),
        '/' => vk(0xBF, 0x35),
        _ => None,
    }
}

/// Символ записи (`uChar`), как его дал бы `ToUnicode`: Ctrl+буква — C0, служебные —
/// свой код, F-клавиши и стрелки — 0.
fn unicode(name: &str, mods: Mods, text: Option<&str>) -> u32 {
    let one = {
        let mut it = name.chars();
        match (it.next(), it.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        }
    };
    if let Some(c) = one {
        let lower = c.to_ascii_lowercase();
        if mods.ctrl {
            return match lower {
                'a'..='z' => u32::from(lower) - u32::from('a') + 1,
                '[' => 0x1B,
                '\\' => 0x1C,
                ']' => 0x1D,
                ' ' => 0x20,
                _ => 0,
            };
        }
        if let Some(t) = text.and_then(|t| t.chars().next()) {
            return u32::from(t);
        }
        return u32::from(if mods.shift {
            c.to_ascii_uppercase()
        } else {
            lower
        });
    }
    match name {
        "enter" if mods.ctrl => 0x0A,
        "enter" => 0x0D,
        "tab" => 0x09,
        "backspace" if mods.ctrl => 0x7F,
        "backspace" => 0x08,
        "escape" => 0x1B,
        "space" => 0x20,
        _ => 0,
    }
}

/// Запись `CSI Vk;Sc;Uc;Kd;Cs;Rc _`.
fn record(out: &mut Vec<u8>, k: Vk, uc: u32, down: bool, cs: u32) {
    use std::io::Write as _;
    let _ = write!(
        out,
        "\x1b[{};{};{};{};{};1_",
        k.vk,
        k.sc,
        uc,
        u8::from(down),
        cs
    );
}

/// Клавиша → нажатие и отпускание; `false` — клавиша неизвестна (тогда кодировать
/// как VT).
pub fn key(name: &str, mods: Mods, text: Option<&str>, out: &mut Vec<u8>) -> bool {
    let Some(k) = lookup(name) else {
        return false;
    };
    let mut cs = 0;
    if mods.shift {
        cs |= SHIFT;
    }
    if mods.ctrl {
        cs |= LEFT_CTRL;
    }
    if mods.alt {
        cs |= LEFT_ALT;
    }
    if k.enhanced {
        cs |= ENHANCED;
    }
    let uc = unicode(name, mods, text);
    record(out, k, uc, true, cs);
    record(out, k, uc, false, cs);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(name: &str, mods: Mods) -> String {
        let mut out = Vec::new();
        assert!(key(name, mods, None, &mut out), "{name}");
        String::from_utf8(out).unwrap()
    }

    fn m(shift: bool, alt: bool, ctrl: bool) -> Mods {
        Mods { shift, alt, ctrl }
    }

    /// Нажатие из пары (отпускание — то же с `Kd=0`).
    fn down(name: &str, mods: Mods) -> String {
        let s = enc(name, mods);
        let (d, u) = s.split_at(s.find('_').unwrap() + 1);
        let mut f: Vec<&str> = d.split(';').collect();
        f[3] = "0";
        assert_eq!(u, f.join(";"));
        d.to_owned()
    }

    /// Табличный тест (S6-20): эталоны — то, что шлёт Windows Terminal.
    #[test]
    fn table() {
        let n = Mods::NONE;
        let cases: &[(&str, Mods, &str)] = &[
            ("space", m(false, false, true), "\x1b[32;57;32;1;8;1_"),
            ("f7", m(false, true, false), "\x1b[118;65;0;1;2;1_"),
            ("f1", m(true, false, false), "\x1b[112;59;0;1;16;1_"),
            ("f4", m(true, false, false), "\x1b[115;62;0;1;16;1_"),
            ("f10", n, "\x1b[121;68;0;1;0;1_"),
            ("f11", n, "\x1b[122;87;0;1;0;1_"),
            ("f12", m(true, false, true), "\x1b[123;88;0;1;24;1_"),
            ("up", n, "\x1b[38;72;0;1;256;1_"),
            ("left", m(false, false, true), "\x1b[37;75;0;1;264;1_"),
            ("delete", n, "\x1b[46;83;0;1;256;1_"),
            ("pagedown", m(true, false, false), "\x1b[34;81;0;1;272;1_"),
            ("enter", n, "\x1b[13;28;13;1;0;1_"),
            ("enter", m(false, false, true), "\x1b[13;28;10;1;8;1_"),
            ("escape", n, "\x1b[27;1;27;1;0;1_"),
            ("tab", m(true, false, false), "\x1b[9;15;9;1;16;1_"),
            ("backspace", n, "\x1b[8;14;8;1;0;1_"),
            ("a", m(false, false, true), "\x1b[65;30;1;1;8;1_"),
            ("z", m(false, false, true), "\x1b[90;44;26;1;8;1_"),
            ("q", m(false, true, false), "\x1b[81;16;113;1;2;1_"),
            ("1", m(false, false, true), "\x1b[49;2;0;1;8;1_"),
            ("0", m(false, true, false), "\x1b[48;11;48;1;2;1_"),
            ("[", m(false, false, true), "\x1b[219;26;27;1;8;1_"),
        ];
        for (name, mods, want) in cases {
            assert_eq!(down(name, *mods), *want, "{name} {mods:?}");
        }
    }

    #[test]
    fn down_then_up() {
        assert_eq!(
            enc("f7", m(false, true, false)),
            "\x1b[118;65;0;1;2;1_\x1b[118;65;0;0;2;1_"
        );
        let mut out = Vec::new();
        assert!(!key("nosuchkey", Mods::NONE, None, &mut out));
        assert_eq!(out, Vec::<u8>::new());
    }

    #[test]
    fn letters_and_digits_cover_layout() {
        for c in 'a'..='z' {
            let k = lookup(&c.to_string()).unwrap();
            assert_eq!(k.vk, c.to_ascii_uppercase() as u16);
            assert!(k.sc > 0 && !k.enhanced);
        }
        for (i, c) in "1234567890".chars().enumerate() {
            let k = lookup(&c.to_string()).unwrap();
            assert_eq!(k.vk, c as u16);
            assert_eq!(usize::from(k.sc), i + 2);
        }
        assert_eq!(lookup("f24").unwrap().vk, 0x87);
        assert!(lookup("f25").is_none());
    }
}
