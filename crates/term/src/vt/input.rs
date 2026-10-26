//! Кодировщик ввода (S6 §3.5, S6-05): клавиши, вставка, фокус и мышь → байты
//! xterm. Не зависит от GPUI: клавиша — имя, как в `Keystroke` gpui-kit
//! (`up`, `f5`, `enter`, `a`…; S0 v3 №13 в редакции TASKS-M3), режимы терминала —
//! флаги [`Modes`] (их ведёт эмулятор).
//!
//! Модификаторы в CSI: `1 + shift + 2·alt + 4·ctrl` (`CSI 1;5A` — Ctrl+↑).
//! Ctrl+буква → C0, Alt → префикс ESC. `AltGr` на Windows приходит как Ctrl+Alt с
//! готовым текстом — тогда отправляется текст как есть.

/// Модификаторы клавиши.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        shift: false,
        alt: false,
        ctrl: false,
    };

    /// Параметр модификатора xterm; `None` — без модификаторов.
    fn param(self) -> Option<u8> {
        let m = u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl);
        (m > 0).then_some(m + 1)
    }
}

/// Режимы терминала, влияющие на ввод.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools, reason = "независимые режимы DEC")]
pub struct Modes {
    /// DECCKM: стрелки и Home/End — `SS3`.
    pub app_cursor: bool,
    /// DEC 2004: вставка в `ESC[200~ … ESC[201~`.
    pub bracketed_paste: bool,
    /// DEC 1004: `CSI I` / `CSI O`.
    pub focus_events: bool,
    /// Отчёты мыши SGR (DEC 1006); иначе — X10 (`CSI M` с байтами).
    pub sgr_mouse: bool,
    /// Приложение просит отчёты мыши (DEC 1000/1002/1003): клики уходят в PTY,
    /// выделяет Shift+мышь (S6 §3.4).
    pub mouse_report: bool,
    /// Отчёты протяжки с нажатой кнопкой (DEC 1002).
    pub mouse_drag: bool,
    /// Отчёты любого движения (DEC 1003).
    pub mouse_motion: bool,
    /// Альтернативный экран.
    pub alt_screen: bool,
    /// DEC 1007: в alt-screen колесо — стрелки.
    pub alt_scroll: bool,
    /// `CSI ?9001h` от `ConPTY` (S6-20): клавиши — записями `CSI Vk;Sc;Uc;Kd;Cs;Rc _`.
    pub win32_input: bool,
}

const ESC: u8 = 0x1b;

fn csi(out: &mut Vec<u8>, body: &str) {
    out.extend_from_slice(b"\x1b[");
    out.extend_from_slice(body.as_bytes());
}

/// Клавиша → байты. `text` — что напечатала бы клавиша (с учётом раскладки и
/// Shift); `None` — непечатная. Возвращает `false`, если клавише нечего слать.
pub fn key(name: &str, mods: Mods, text: Option<&str>, term: Modes, out: &mut Vec<u8>) -> bool {
    // AltGr (Windows): Ctrl+Alt с напечатанным символом — сам символ.
    if mods.ctrl
        && mods.alt
        && let Some(t) = text.filter(|t| !t.is_empty() && !t.chars().any(char::is_control))
    {
        out.extend_from_slice(t.as_bytes());
        return true;
    }
    if let Some(done) = special(name, mods, term, out) {
        return done;
    }
    if mods.ctrl
        && let Some(b) = ctrl_byte(name, text)
    {
        if mods.alt {
            out.push(ESC);
        }
        out.push(b);
        return true;
    }
    let Some(t) = text.filter(|t| !t.is_empty()) else {
        return false;
    };
    if mods.alt {
        out.push(ESC);
    }
    out.extend_from_slice(t.as_bytes());
    true
}

/// Ctrl+клавиша → управляющий байт C0.
fn ctrl_byte(name: &str, text: Option<&str>) -> Option<u8> {
    let c = if name.chars().count() == 1 {
        name.chars().next()?
    } else if name == "space" {
        ' '
    } else {
        text.and_then(|t| t.chars().next())?
    };
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        ' ' | '@' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '-' | '7' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

/// Непечатные клавиши; `None` — не из них.
fn special(name: &str, mods: Mods, term: Modes, out: &mut Vec<u8>) -> Option<bool> {
    let m = mods.param();
    // Стрелки, Home, End: без модификаторов — SS3 в APP_CURSOR, иначе CSI.
    let cursor = match name {
        "up" => Some('A'),
        "down" => Some('B'),
        "right" => Some('C'),
        "left" => Some('D'),
        "home" => Some('H'),
        "end" => Some('F'),
        _ => None,
    };
    if let Some(c) = cursor {
        match m {
            Some(m) => csi(out, &format!("1;{m}{c}")),
            None if term.app_cursor => {
                out.extend_from_slice(b"\x1bO");
                out.push(c as u8);
            }
            None => csi(out, &c.to_string()),
        }
        return Some(true);
    }
    // F1–F4: SS3 P–S, с модификатором — CSI 1;m P–S.
    let f14 = match name {
        "f1" => Some('P'),
        "f2" => Some('Q'),
        "f3" => Some('R'),
        "f4" => Some('S'),
        _ => None,
    };
    if let Some(c) = f14 {
        if let Some(m) = m {
            csi(out, &format!("1;{m}{c}"));
        } else {
            out.extend_from_slice(b"\x1bO");
            out.push(c as u8);
        }
        return Some(true);
    }
    // `CSI n ~` (с модификатором — `CSI n;m ~`).
    let tilde = match name {
        "insert" => Some(2),
        "delete" => Some(3),
        "pageup" => Some(5),
        "pagedown" => Some(6),
        "f5" => Some(15),
        "f6" => Some(17),
        "f7" => Some(18),
        "f8" => Some(19),
        "f9" => Some(20),
        "f10" => Some(21),
        "f11" => Some(23),
        "f12" => Some(24),
        _ => None,
    };
    if let Some(n) = tilde {
        match m {
            Some(m) => csi(out, &format!("{n};{m}~")),
            None => csi(out, &format!("{n}~")),
        }
        return Some(true);
    }
    let alt = |out: &mut Vec<u8>| {
        if mods.alt {
            out.push(ESC);
        }
    };
    match name {
        "enter" => {
            alt(out);
            out.push(b'\r');
        }
        "tab" if mods.shift => csi(out, "Z"),
        "tab" => {
            alt(out);
            out.push(b'\t');
        }
        "backspace" => {
            alt(out);
            out.push(if mods.ctrl { 0x08 } else { 0x7f });
        }
        "escape" => {
            alt(out);
            out.push(ESC);
        }
        _ => return None,
    }
    Some(true)
}

/// Вставка: в режиме DEC 2004 — в скобках (без `ESC` внутри, чтобы вставка не
/// закрыла скобки сама); иначе `\r\n` и `\n` → `\r`.
pub fn paste(text: &str, term: Modes, out: &mut Vec<u8>) {
    if term.bracketed_paste {
        out.extend_from_slice(b"\x1b[200~");
        out.extend(text.bytes().filter(|&b| b != ESC));
        out.extend_from_slice(b"\x1b[201~");
    } else {
        let t = text.replace("\r\n", "\r").replace('\n', "\r");
        out.extend_from_slice(t.as_bytes());
    }
}

/// Фокус окна: `CSI I` / `CSI O` (только в режиме DEC 1004).
pub fn focus(gained: bool, term: Modes, out: &mut Vec<u8>) {
    if term.focus_events {
        csi(out, if gained { "I" } else { "O" });
    }
}

/// Кнопка мыши.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    /// Движение без нажатых кнопок.
    None,
}

/// Событие мыши в ячейках (с 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseEvent {
    pub button: MouseButton,
    pub pressed: bool,
    pub motion: bool,
    pub col: u16,
    pub row: u16,
    pub mods: Mods,
}

/// Отчёт мыши: SGR (`CSI < b;x;y M|m`) или X10 (`CSI M` + три байта, до 223).
pub fn mouse(e: MouseEvent, term: Modes, out: &mut Vec<u8>) -> bool {
    let mut b: u16 = match e.button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::None => 3,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
    };
    if e.mods.shift {
        b += 4;
    }
    if e.mods.alt {
        b += 8;
    }
    if e.mods.ctrl {
        b += 16;
    }
    if e.motion {
        b += 32;
    }
    if term.sgr_mouse {
        let end = if e.pressed { 'M' } else { 'm' };
        csi(out, &format!("<{b};{};{}{end}", e.col, e.row));
        return true;
    }
    // X10: отпускание — кнопка 3; координаты кодируются байтом 32 + n (n ≤ 223).
    if !e.pressed && !matches!(e.button, MouseButton::WheelUp | MouseButton::WheelDown) {
        b = (b & !3) | 3;
    }
    let (Ok(x), Ok(y)) = (u8::try_from(e.col), u8::try_from(e.row)) else {
        return false;
    };
    if x > 223 || y > 223 {
        return false;
    }
    csi(out, "M");
    out.push(32 + u8::try_from(b).unwrap_or(0));
    out.push(32 + x);
    out.push(32 + y);
    true
}

#[cfg(test)]
#[path = "input_tests.rs"]
mod tests;
