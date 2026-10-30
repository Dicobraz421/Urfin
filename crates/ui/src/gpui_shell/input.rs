//! Клавиатура GPUI → конвейер keymap S3 (G-03). Нажатия перехватываются на уровне
//! приложения (`intercept_keystrokes`, до привязок gpui-kit) и переводятся в события
//! `urfin_keymap::ev::RawInput`, которые разбирает [`KeyInput::process`]: пресеты, аккорды,
//! раскладка-независимость и быстрый поиск работают как в egui-окне.
//!
//! На Windows subclass окна (`sys::key_notes`) пишет записки `KeyNotes` в очередь этого
//! же `KeyInput` — их разбирает каждый `process`; записки без нажатия gpui (`Num5` без
//! `NumLock`) — [`Input::flush`] по будильнику subclass. Он же на время разбора клавиши
//! отдаёт [`KeyMsg`] — скан-код и надпись клавиши, которых нет в `Keystroke`: по ним
//! сочетание не зависит от раскладки (v4 №5, [`pressed`]).

use std::time::Instant;

use gpui_kit::{Keystroke, Modifiers};
use urfin_core::{AppState, Command};
use urfin_keymap::ev::{Event, Key, Modifiers as EMods, RawInput};
use urfin_keymap::{ActiveKeymap, KeyInput, KeyNotes};

/// Сообщение клавиши Windows, которое gpui разбирает прямо сейчас (`sys::key_notes`):
/// то, чего нет в `Keystroke`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct KeyMsg {
    /// Код клавиши `VK_*` (`wParam`).
    pub vk: u16,
    /// Скан-код (биты 16–23 `lParam`): место клавиши, от раскладки не зависит.
    pub scan: u8,
    /// Бит 24 `lParam`: расширенная клавиша (стрелки, серый `/`, `NumEnter`).
    pub extended: bool,
    /// Shift зажат. gpui снимает Shift, когда меняет клавишу на символ (Shift+1 → `!`).
    pub shift: bool,
    /// Надпись клавиши без модификаторов в текущей раскладке (`MapVirtualKeyW`,
    /// `MAPVK_VK_TO_CHAR`): у букв — латиница при любой раскладке, у прочих — символ
    /// раскладки (`х` на месте `[` в русской).
    pub label: Option<char>,
}

/// Клавиша нажатия для keymap.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Pressed {
    key: Option<Key>,
    physical: Option<Key>,
    /// Shift, в том числе снятый gpui.
    shift: bool,
}

/// Состояние моста: конвейер keymap и последние модификаторы.
pub(super) struct Input {
    keys: KeyInput,
    /// Очередь записок `keys` (её же пишет subclass окна).
    notes: KeyNotes,
    mods: EMods,
}

/// Итог нажатия.
pub(super) struct Outcome {
    /// keymap забрал событие: дальше по gpui не пускать.
    pub consumed: bool,
    /// Дедлайн аккорда: к нему надо разбудить `pump`.
    pub wake_at: Option<Instant>,
}

impl Input {
    pub(super) fn new(keys: KeyInput) -> Self {
        Input {
            notes: keys.notes(),
            keys,
            mods: EMods::NONE,
        }
    }

    /// Очередь записок для subclass окна (`sys::key_notes`).
    pub(super) fn notes(&self) -> KeyNotes {
        self.notes.clone()
    }

    /// Нажатие разобрал не конвейер (поле ввода, меню, вопрос): его записки — сбросить.
    pub(super) fn skip(&self) {
        self.notes.clear();
    }

    /// Перечитанная раскладка (`keymap.reload`, `app.reload_config`) — в конвейер.
    fn sync(&mut self, st: &AppState) {
        if let Some(a) = st.ext.get::<ActiveKeymap>()
            && a.generation != self.keys.generation()
        {
            self.keys
                .apply(std::sync::Arc::clone(&a.keymap), &a.cfg, a.generation);
        }
    }

    fn run(&mut self, events: Vec<Event>, st: &AppState, out: &mut Vec<Command>) -> Outcome {
        self.sync(st);
        let n = events.len();
        let before = out.len();
        let mut raw = RawInput { events };
        let now = Instant::now();
        let o = self.keys.process(&mut raw, st, now, out);
        Outcome {
            // Без событий (Ctrl+Gray*) сочетание дала записка: поглощено, если есть команды.
            consumed: raw.events.len() < n || (n == 0 && out.len() > before),
            wake_at: o.wake_at,
        }
    }

    /// Записки subclass, не встретившие нажатия gpui, — в конвейер (синтетические
    /// сочетания `Num5`, стороны модификаторов).
    pub(super) fn flush(&mut self, st: &AppState, out: &mut Vec<Command>) -> Outcome {
        self.run(Vec::new(), st, out)
    }

    /// Нажатие: клавиша (по надписи или по месту — [`pressed`]) и введённый символ.
    pub(super) fn key_down(
        &mut self,
        ks: &Keystroke,
        st: &AppState,
        out: &mut Vec<Command>,
    ) -> Outcome {
        self.key_down_with(ks, key_msg(), st, out)
    }

    fn key_down_with(
        &mut self,
        ks: &Keystroke,
        msg: Option<KeyMsg>,
        st: &AppState,
        out: &mut Vec<Command>,
    ) -> Outcome {
        let p = pressed(ks, msg);
        self.mods = mods(ks.modifiers, p.shift);
        let mut events = Vec::with_capacity(2);
        if let Some(key) = p.key {
            events.push(Event::Key {
                key,
                physical_key: p.physical,
                pressed: true,
                repeat: false,
                modifiers: self.mods,
            });
        }
        if let Some(text) = ks.key_char.as_deref()
            && typed(ks.modifiers, text)
        {
            events.push(Event::Text(text.to_owned()));
        }
        // Пусто (Ctrl+Gray*: у gpui нет клавиши `*`, символа при Ctrl нет) — всё равно
        // `process`: сочетание даст записка subclass.
        self.run(events, st, out)
    }

    pub(super) fn key_up(&mut self, ks: &Keystroke, st: &AppState, out: &mut Vec<Command>) {
        self.key_up_with(ks, key_msg(), st, out);
    }

    /// Отпускание — та же клавиша, что у нажатия: иначе keymap счёл бы следующее нажатие
    /// повтором.
    fn key_up_with(
        &mut self,
        ks: &Keystroke,
        msg: Option<KeyMsg>,
        st: &AppState,
        out: &mut Vec<Command>,
    ) {
        let p = pressed(ks, msg);
        self.mods = mods(ks.modifiers, p.shift);
        if let Some(key) = p.key {
            let ev = Event::Key {
                key,
                physical_key: p.physical,
                pressed: false,
                repeat: false,
                modifiers: self.mods,
            };
            self.run(vec![ev], st, out);
        }
    }

    /// Модификаторы без клавиши: одиночный Alt (меню F9), аккорды на Ctrl.
    pub(super) fn modifiers(
        &mut self,
        m: Modifiers,
        st: &AppState,
        out: &mut Vec<Command>,
    ) -> Outcome {
        self.mods = mods(m, m.shift);
        self.run(vec![Event::ModifiersChanged(self.mods)], st, out)
    }

    /// Окно потеряло фокус: сбросить аккорд и зажатые модификаторы.
    pub(super) fn focus_lost(&mut self, st: &AppState, out: &mut Vec<Command>) {
        self.mods = EMods::NONE;
        self.run(vec![Event::WindowFocused(false)], st, out);
    }
}

fn mods(m: Modifiers, shift: bool) -> EMods {
    EMods {
        alt: m.alt,
        ctrl: m.control,
        shift,
        mac_cmd: false,
        command: m.control,
    }
}

/// Нажатие даёт символ (`Text`): без Ctrl или с `AltGr` (на Windows — Ctrl+Alt). С Alt —
/// тоже: Alt+буква начинает быстрый поиск Far, символ — из `Text` (v4 №5). Кроме
/// пробела: Alt+пробел — меню окна.
fn typed(m: Modifiers, text: &str) -> bool {
    if text.chars().any(char::is_control) {
        return false;
    }
    match (m.control, m.alt) {
        (true, alt) => alt,
        (false, true) => !text.chars().any(char::is_whitespace),
        (false, false) => true,
    }
}

/// Клавиша нажатия так, как её видит keymap (по надписи или по месту, v4 №5): строки фич
/// (Alt+C в «Найти») ловят букву на любой раскладке.
pub(super) fn stroke_key(ks: &Keystroke) -> Option<Key> {
    pressed(ks, key_msg()).key
}

/// Сообщение клавиши, которое сейчас разбирает gpui (Windows).
fn key_msg() -> Option<KeyMsg> {
    #[cfg(windows)]
    return crate::sys::key_notes::key_msg();
    #[cfg(not(windows))]
    None
}

/// Клавиша нажатия по v4 №5 (S3 §1 №1 — как `logical.or(physical)` у egui-winit): надпись
/// в раскладке, если это клавиша keymap (латиница; Dvorak, AZERTY, QWERTZ — «по надписи»),
/// иначе — место в US-QWERTY по скан-коду (кириллица, греческий, иврит, арабский: `х` →
/// `[`; мёртвая `^` AZERTY → `[`). Надпись букв gpui берёт из кода клавиши: она латинская
/// при любой раскладке.
///
/// Клавиша со Shift — надпись без Shift и Shift: gpui отдаёт символ (`{`, `!`, `Х`) и
/// снимает Shift, а пресеты пишут `Ctrl+Shift+OpenBracket`, `Ctrl+Shift+1`.
///
/// Физическая `Plus` в keymap — серый `+` (S3-07, `GrayPlus`): она только у `VK_ADD`.
/// Сообщения нет (subclass не стоит) — по имени gpui, как раньше.
fn pressed(ks: &Keystroke, msg: Option<KeyMsg>) -> Pressed {
    let Some(m) = msg else {
        let key = key_of(&ks.key);
        return Pressed {
            key,
            physical: key,
            shift: ks.modifiers.shift,
        };
    };
    let shifted = m.shift && !ks.modifiers.shift;
    let label = if shifted {
        m.label.and_then(|c| key_of(c.encode_utf8(&mut [0; 4])))
    } else {
        key_of(&ks.key)
    };
    let place = place_of(m.scan, m.extended);
    let key = label.or(place);
    let physical = if m.vk == urfin_keymap::input::vk::ADD {
        Some(Key::Plus)
    } else {
        match place {
            Some(p) => Some(p),
            None if key == Some(Key::Plus) => None,
            None => key,
        }
    };
    Pressed {
        key,
        physical,
        shift: ks.modifiers.shift || shifted,
    }
}

/// Клавиша US-QWERTY на месте скан-кода основного блока (буквы, цифры, символы). Прочие
/// клавиши (F1…, стрелки, цифровой блок, Enter, пробел) gpui называет сам.
fn place_of(scan: u8, extended: bool) -> Option<Key> {
    /// Ряды: первый скан-код и клавиши подряд.
    const ROWS: [(u8, &str); 4] = [
        (0x02, "1234567890-="),
        (0x10, "qwertyuiop[]"),
        (0x1E, "asdfghjkl;'`"),
        (0x2B, "\\zxcvbnm,./"),
    ];
    if extended {
        return None;
    }
    let (first, row) = ROWS.iter().rev().find(|(s, _)| scan >= *s)?;
    let c = row.chars().nth(usize::from(scan - first))?;
    key_of(c.encode_utf8(&mut [0; 4]))
}

/// Имя клавиши gpui (`"down"`, `"pagedown"`, `"a"`, `"f5"`, `"-"`) → клавиша keymap.
pub(super) fn key_of(name: &str) -> Option<Key> {
    let named = match name {
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "home" => "Home",
        "end" => "End",
        "enter" => "Enter",
        "escape" => "Escape",
        "tab" => "Tab",
        "backspace" => "Backspace",
        "insert" => "Insert",
        "delete" => "Delete",
        "space" => "Space",
        _ => "",
    };
    if !named.is_empty() {
        return Key::from_name(named);
    }
    // F1…F35.
    if let Some(n) = name.strip_prefix('f')
        && !n.is_empty()
        && n.bytes().all(|b| b.is_ascii_digit())
    {
        return Key::from_name(&format!("F{n}"));
    }
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphabetic() => {
            Key::from_name(&c.to_ascii_uppercase().to_string())
        }
        (Some(c), None) => Key::from_name(&c.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpui_names_map_to_egui_keys() {
        for (g, k) in [
            ("down", Key::ArrowDown),
            ("pagedown", Key::PageDown),
            ("enter", Key::Enter),
            ("tab", Key::Tab),
            ("insert", Key::Insert),
            ("space", Key::Space),
            ("f5", Key::F5),
            ("f10", Key::F10),
            ("a", Key::A),
            ("q", Key::Q),
            ("7", Key::Num7),
            ("-", Key::Minus),
            ("=", Key::Equals),
            ("[", Key::OpenBracket),
            ("]", Key::CloseBracket),
            ("\\", Key::Backslash),
        ] {
            assert_eq!(key_of(g), Some(k), "{g}");
        }
        assert_eq!(key_of("shift"), None);
    }

    fn ks(key: &str, m: Modifiers) -> Keystroke {
        Keystroke {
            modifiers: m,
            key: key.into(),
            key_char: None,
        }
    }

    fn msg(vk: u16, scan: u8, shift: bool, label: char) -> KeyMsg {
        KeyMsg {
            vk,
            scan,
            extended: false,
            shift,
            label: Some(label),
        }
    }

    /// Серые клавиши gpui называет как основной ряд; различает их записка subclass.
    /// Серый `+` — по коду клавиши: `"+"` основного ряда (Shift+=, `+` QWERTZ) не
    /// `GrayPlus`.
    #[test]
    fn numpad_names_and_gray_plus() {
        for (g, k) in [
            ("-", Key::Minus),
            ("/", Key::Slash),
            ("+", Key::Plus),
            ("enter", Key::Enter),
            ("delete", Key::Delete),
        ] {
            assert_eq!(key_of(g), Some(k), "{g}");
        }
        let none = Modifiers::default();
        let p = |key, m: Option<KeyMsg>| {
            let p = pressed(&ks(key, none), m);
            (p.key, p.physical, p.shift)
        };
        let (add, sub) = (
            urfin_keymap::input::vk::ADD,
            urfin_keymap::input::vk::SUBTRACT,
        );
        let plus = Some(Key::Plus);
        assert_eq!(
            p("+", Some(msg(add, 0x4E, false, '+'))),
            (plus, plus, false)
        );
        assert_eq!(
            p("+", Some(msg(0xBB, 0x0D, true, '='))),
            (Some(Key::Equals), Some(Key::Equals), true),
            "Shift+= US"
        );
        assert_eq!(
            p("+", Some(msg(0xBB, 0x1B, false, '+'))),
            (plus, Some(Key::CloseBracket), false),
            "+ QWERTZ"
        );
        assert_eq!(p("+", None), (plus, plus, false));
        let minus = Some(Key::Minus);
        assert_eq!(
            p("-", Some(msg(sub, 0x4A, false, '-'))),
            (minus, minus, false)
        );
    }

    /// Места скан-кодов основного блока — US-QWERTY; прочие клавиши и расширенные — нет.
    #[test]
    fn scan_places() {
        for (scan, k) in [
            (0x02, Key::Num1),
            (0x0B, Key::Num0),
            (0x0D, Key::Equals),
            (0x10, Key::Q),
            (0x1A, Key::OpenBracket),
            (0x1B, Key::CloseBracket),
            (0x1E, Key::A),
            (0x27, Key::Semicolon),
            (0x28, Key::Quote),
            (0x29, Key::Backtick),
            (0x2B, Key::Backslash),
            (0x2E, Key::C),
            (0x35, Key::Slash),
        ] {
            assert_eq!(place_of(scan, false), Some(k), "{scan:#x}");
        }
        // Esc, Backspace, Tab, Enter, Ctrl, LShift, RShift, серая `*`, пробел, `<>` ISO,
        // серый `+`, японские `ろ` и `¥`.
        for scan in [
            0x01, 0x0E, 0x0F, 0x1C, 0x1D, 0x2A, 0x36, 0x37, 0x39, 0x56, 0x4E, 0x73, 0x7D,
        ] {
            assert_eq!(place_of(scan, false), None, "{scan:#x}");
        }
        assert_eq!(place_of(0x35, true), None, "серый /");
    }

    /// Alt+буква даёт символ (быстрый поиск), Ctrl+буква — нет; `AltGr` — символ.
    #[test]
    fn typed_text() {
        let m = |control, alt| Modifiers {
            control,
            alt,
            ..Modifiers::default()
        };
        assert!(typed(m(false, false), "ф"));
        assert!(typed(m(false, true), "ф"));
        assert!(!typed(m(true, false), "ф"));
        assert!(typed(m(true, true), "€"), "AltGr");
        assert!(!typed(m(false, true), " "), "Alt+пробел — меню окна");
        assert!(typed(m(false, false), " "));
        assert!(!typed(m(false, false), "\u{1b}"));
    }
}

#[cfg(test)]
#[path = "input_matrix_tests.rs"]
mod matrix_tests;
