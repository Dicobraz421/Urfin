//! Конвейер ввода (S3 §1.2): события `RawInput` кадра → сочетания → команды.
//!
//! [`KeyInput::process`] вызывается в `App::raw_input_hook`: поглощённые события
//! удаляются, egui видит только остаток. Повторы считаем сами: egui помечает `repeat`
//! уже после хука. Записки [`KeyNotes`] (Windows, S3-07) уточняют то, что egui-winit
//! теряет: цифровой блок, `Ctrl+Ins`/`Shift+Ins`/`Shift+Del`, стороны модификаторов.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::ev::{Event, ImeEvent, Key};
use compact_str::CompactString;
use smallvec::SmallVec;
use urfin_core::{AppState, Command, Focus, Origin, OverlayKind};

use crate::{
    Chord, ChordState, KeyCode, Keymap, KeymapCfg, Lookup, MatchMode, Mods, Os, QsMode, Stroke,
};

/// Коды клавиш Windows, которые пишет хук (S3 §0, S3-07).
pub mod vk {
    pub const CLEAR: u16 = 0x0C;
    pub const RETURN: u16 = 0x0D;
    pub const CONTROL: u16 = 0x11;
    pub const MENU: u16 = 0x12;
    pub const INSERT: u16 = 0x2D;
    pub const DELETE: u16 = 0x2E;
    pub const MULTIPLY: u16 = 0x6A;
    pub const ADD: u16 = 0x6B;
    pub const SUBTRACT: u16 = 0x6D;
    pub const DIVIDE: u16 = 0x6F;
    pub const LCONTROL: u16 = 0xA2;
    pub const RCONTROL: u16 = 0xA3;
    pub const LMENU: u16 = 0xA4;
    pub const RMENU: u16 = 0xA5;
}

/// Записка хука: виртуальная клавиша, флаг extended и модификаторы со сторонами.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct KeyNote {
    pub vk: u16,
    pub extended: bool,
    pub mods: Mods,
    pub down: bool,
}

/// Clone + Send. Пишет хук Windows, читает [`KeyInput`].
#[derive(Clone, Debug, Default)]
pub struct KeyNotes(Arc<Mutex<VecDeque<KeyNote>>>);

impl KeyNotes {
    /// Очередь ограничена: если кадры не идут, старые записки не копятся.
    const CAP: usize = 64;

    pub fn push(&self, n: KeyNote) {
        let mut q = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if q.len() == Self::CAP {
            q.pop_front();
        }
        q.push_back(n);
    }

    /// Сбросить записки: нажатие разобрал не конвейер (поле ввода, меню, вопрос) —
    /// иначе записка позже дала бы синтетическое сочетание.
    pub fn clear(&self) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    fn drain_into(&self, out: &mut Vec<(KeyNote, bool)>) {
        let mut q = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        out.extend(q.drain(..).map(|n| (n, false)));
    }
}

/// Итог кадра.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct InputOutcome {
    /// Когда разбудить UI: дедлайн аккорда.
    pub wake_at: Option<Instant>,
}

/// Модификаторы: общие — из egui, стороны (`LCtrl`, `RAlt`…) — из записок хука.
#[derive(Debug, Default)]
struct ModTracker {
    held: Mods,
    sides: Mods,
}

impl ModTracker {
    fn set(&mut self, m: crate::ev::Modifiers) {
        self.held = mods_of(m);
        if !self.held.contains(Mods::CTRL) {
            self.sides.remove(Mods::LCTRL | Mods::RCTRL);
        }
        if !self.held.contains(Mods::ALT) {
            self.sides.remove(Mods::LALT | Mods::RALT);
        }
    }

    /// Записка модификатора обновляет стороны; `false` — записка не о модификаторе.
    fn note(&mut self, n: KeyNote) -> bool {
        let side = match (n.vk, n.extended) {
            (vk::LCONTROL, _) | (vk::CONTROL, false) => Mods::LCTRL,
            (vk::RCONTROL, _) | (vk::CONTROL, true) => Mods::RCTRL,
            (vk::LMENU, _) | (vk::MENU, false) => Mods::LALT,
            (vk::RMENU, _) | (vk::MENU, true) => Mods::RALT,
            _ => return false,
        };
        self.sides.set(side, n.down);
        true
    }

    fn with_sides(&self, m: Mods) -> Mods {
        let mut sides = self.sides;
        if !m.contains(Mods::CTRL) {
            sides.remove(Mods::LCTRL | Mods::RCTRL);
        }
        if !m.contains(Mods::ALT) {
            sides.remove(Mods::LALT | Mods::RALT);
        }
        m | sides
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Клавиша печатает символ (буквы, цифры, знаки, пробел), а не управляет.
fn prints(key: Key) -> bool {
    !matches!(
        key,
        Key::ArrowDown
            | Key::ArrowLeft
            | Key::ArrowRight
            | Key::ArrowUp
            | Key::Escape
            | Key::Tab
            | Key::Backspace
            | Key::Enter
            | Key::Insert
            | Key::Delete
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown
            | Key::Copy
            | Key::Cut
            | Key::Paste
    ) && !key
        .name()
        .strip_prefix('F')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn mods_of(m: crate::ev::Modifiers) -> Mods {
    let mut out = Mods::empty();
    out.set(Mods::CTRL, m.ctrl);
    out.set(Mods::ALT, m.alt);
    out.set(Mods::SHIFT, m.shift);
    out.set(Mods::CMD, m.mac_cmd);
    out
}

/// Нажатая клавиша: прошло ли хоть одно её нажатие в egui (тогда и отпускание — туда).
#[derive(Copy, Clone, Debug)]
struct Down {
    key: Key,
    passed: bool,
}

#[derive(Debug)]
/// Конвейер ввода: keymap, модификаторы, аккорд, IME, `suppress_text`, записки.
pub struct KeyInput {
    km: Arc<Keymap>,
    os: Os,
    r#match: MatchMode,
    /// Как начинается быстрый поиск (`auto` уже разрешён по пресету).
    qs_mode: QsMode,
    alt_tap: Duration,
    /// Нажат один модификатор (`ALT` или `CTRL`) — кандидат в `AltTap`/`CtrlTap`.
    tap: Option<(Mods, Instant)>,
    /// Начатый аккорд, о котором уже сообщено (`keymap.pending`).
    shown_pending: Option<CompactString>,
    /// Поколение `ActiveKeymap`, с которым собран конвейер.
    generation: u64,
    /// Сочетание без уточнения запиской (`Enter` для `NumEnter`): ищется, если у
    /// уточнённого нет привязки. Живёт один `feed`.
    fallback: Option<Stroke>,
    chord_timeout: Duration,
    mods: ModTracker,
    pending: Option<(ChordState, Instant)>,
    ime: bool,
    suppress_text: bool,
    down: SmallVec<[Down; 8]>,
    notes: KeyNotes,
    frame_notes: Vec<(KeyNote, bool)>,
    /// Захват сочетания (раздел «Клавиатура»): нажатия не идут в раскладку.
    capture: bool,
    captured: Option<Stroke>,
    /// Клавиша захвата ещё зажата: её автоповтор не должен сработать как команда.
    swallow: Option<Key>,
}

/// Как начинается быстрый поиск: Far — Alt+буква, DN — одиночный Alt (S3 §1.3).
pub fn qs_mode(cfg: &KeymapCfg) -> QsMode {
    match cfg.quick_search {
        QsMode::Auto if cfg.preset == "dn" => QsMode::AltTap,
        QsMode::Auto => QsMode::Alt,
        m => m,
    }
}

impl KeyInput {
    pub fn new(km: Arc<Keymap>, cfg: &KeymapCfg) -> Self {
        let qs_mode = qs_mode(cfg);
        KeyInput {
            os: km.os(),
            km,
            r#match: cfg.r#match,
            qs_mode,
            alt_tap: Duration::from_millis(u64::from(cfg.alt_tap_ms)),
            tap: None,
            shown_pending: None,
            fallback: None,
            generation: 0,
            chord_timeout: Duration::from_millis(u64::from(cfg.chord_timeout_ms)),
            mods: ModTracker::default(),
            pending: None,
            ime: false,
            suppress_text: false,
            down: SmallVec::new(),
            notes: KeyNotes::default(),
            frame_notes: Vec::new(),
            capture: false,
            captured: None,
            swallow: None,
        }
    }

    /// Захват сочетания: пока он включён, нажатия только запоминаются
    /// ([`Self::take_captured`]) — та же клавиша, что нашёл бы поиск (раскладка, записки),
    /// без сторон модификаторов; команд нет, одиночный Alt не срабатывает, текст
    /// поглощается.
    pub fn set_capture(&mut self, on: bool) {
        if on != self.capture {
            self.capture = on;
            self.captured = None;
            self.pending = None;
            self.tap = None;
        }
    }

    pub fn capturing(&self) -> bool {
        self.capture
    }

    /// Захваченное сочетание.
    pub fn take_captured(&mut self) -> Option<Stroke> {
        self.captured.take()
    }

    /// Очередь записок для хука Windows.
    pub fn notes(&self) -> KeyNotes {
        self.notes.clone()
    }

    /// Поколение раскладки (`ActiveKeymap::generation`), с которым работает конвейер.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Новая раскладка и настройки `[keymap]` (`ActiveKeymap`): аккорд, одиночный Alt
    /// и записки сбрасываются, очередь хука остаётся.
    pub fn apply(&mut self, km: Arc<Keymap>, cfg: &KeymapCfg, generation: u64) {
        let notes = self.notes.clone();
        let (capture, swallow) = (self.capture, self.swallow);
        *self = KeyInput::new(km, cfg);
        self.notes = notes;
        self.generation = generation;
        self.capture = capture;
        self.swallow = swallow;
    }

    /// Общая очередь с хуком, поставленным до создания `KeyInput` (app, S3-07).
    pub fn set_notes(&mut self, notes: KeyNotes) {
        self.notes = notes;
    }

    /// Новая раскладка (`keymap.reload`): незавершённый аккорд сбрасывается.
    pub fn set_keymap(&mut self, km: Arc<Keymap>) {
        self.os = km.os();
        self.km = km;
        self.pending = None;
    }

    /// Начатый аккорд (строка статуса, `KeymapView.pending` — S3-06).
    pub fn pending(&self) -> Option<Chord> {
        self.pending.as_ref().map(|(c, _)| Chord(c.strokes.clone()))
    }

    /// Разбирает события кадра. Поглощённые удаляются из `raw.events`, команды
    /// (`Origin::Key`) добавляются в `out`.
    pub fn process(
        &mut self,
        raw: &mut crate::ev::RawInput,
        st: &AppState,
        now: Instant,
        out: &mut Vec<Command>,
    ) -> InputOutcome {
        self.frame_notes.clear();
        self.notes.drain_into(&mut self.frame_notes);
        for (n, used) in &mut self.frame_notes {
            // Отпускание модификатора тоже обновляет стороны.
            let modifier = self.mods.note(*n);
            *used = modifier || !n.down;
        }
        if self.pending.as_ref().is_some_and(|(_, d)| now >= *d) {
            self.pending = None;
        }
        let events = std::mem::take(&mut raw.events);
        raw.events.reserve(events.len());
        for ev in events {
            if self.event(&ev, st, now, out) {
                raw.events.push(ev);
            }
        }
        self.unpaired_notes(raw, st, now, out);
        self.sync_pending(out);
        InputOutcome {
            wake_at: self.pending.as_ref().map(|(_, d)| *d),
        }
    }

    /// `true` — событие остаётся для egui.
    fn event(&mut self, ev: &Event, st: &AppState, now: Instant, out: &mut Vec<Command>) -> bool {
        match ev {
            Event::Ime(ImeEvent::Preedit { text, .. }) => {
                self.ime = !text.is_empty();
                return true;
            }
            Event::Ime(ImeEvent::Commit(t)) => return self.ime_commit(t, st, out),
            Event::WindowFocused(false) => {
                self.mods.reset();
                self.pending = None;
                self.suppress_text = false;
                self.down.clear();
                self.tap = None;
                return true;
            }
            Event::ModifiersChanged(m) => {
                self.modifiers_changed(mods_of(*m), st, now, out);
                self.mods.set(*m);
                return true;
            }
            Event::PointerButton { pressed: true, .. } => {
                self.tap = None;
                // Клик закрывает быстрый поиск (сам клик — виджетам).
                if is_qs(st) && self.km.has_command("keymap.qs.cancel") {
                    out.push(Command::new("keymap.qs.cancel").from(Origin::Key));
                }
                return true;
            }
            Event::MouseWheel { .. } => {
                self.tap = None;
                return true;
            }
            _ => {}
        }
        if self.ime {
            return true;
        }
        match ev {
            Event::Key {
                key,
                physical_key,
                pressed,
                modifiers,
                ..
            } => {
                self.mods.set(*modifiers);
                if *pressed {
                    self.press(*key, *physical_key, *modifiers, st, now, out)
                } else {
                    self.release(*key)
                }
            }
            // Вставка в быстрый поиск: символы, пока есть совпадения (Far).
            Event::Paste(t) if is_qs(st) && self.km.has_command("keymap.qs.input") => {
                self.suppress_text = false;
                out.push(
                    Command::new("keymap.qs.input")
                        .arg("text", t.as_str())
                        .from(Origin::Key),
                );
                false
            }
            Event::Copy | Event::Cut | Event::Paste(_) => {
                self.suppress_text = false;
                let (vk, letter) = match ev {
                    Event::Copy => (vk::INSERT, Key::C),
                    Event::Cut => (vk::DELETE, Key::X),
                    _ => (vk::INSERT, Key::V),
                };
                let stroke = if let Some(n) = self.pair(vk) {
                    let key = if vk == vk::INSERT {
                        Key::Insert
                    } else {
                        Key::Delete
                    };
                    Stroke::new(KeyCode::Key(key), n.mods)
                } else {
                    let m = if self.os == Os::Macos {
                        Mods::CMD
                    } else {
                        Mods::CTRL
                    };
                    Stroke::new(KeyCode::Key(letter), self.mods.with_sides(m))
                };
                let text = match ev {
                    Event::Paste(t) => Some(t.as_str()),
                    _ => None,
                };
                !self.feed(stroke, false, st, now, out, text)
            }
            Event::Text(t) => self.text(t, st, now, out),
            _ => true,
        }
    }

    /// Конец композиции IME. Быстрый поиск без поля egui: итог (CJK) — в маску.
    fn ime_commit(&mut self, t: &str, st: &AppState, out: &mut Vec<Command>) -> bool {
        self.ime = false;
        if is_qs(st) && !t.is_empty() && self.km.has_command("keymap.qs.input") {
            out.push(
                Command::new("keymap.qs.input")
                    .arg("text", t)
                    .from(Origin::Key),
            );
            return false;
        }
        true
    }

    fn press(
        &mut self,
        key: Key,
        physical: Option<Key>,
        modifiers: crate::ev::Modifiers,
        st: &AppState,
        now: Instant,
        out: &mut Vec<Command>,
    ) -> bool {
        self.suppress_text = false;
        self.tap = None;
        if !self.capture && self.swallow == Some(key) {
            return false;
        }
        let i = self.down.iter().position(|d| d.key == key);
        let repeat = i.is_some();
        let (code, base) = self.code(key, physical);
        let mods = self.mods.with_sides(mods_of(modifiers));
        // `NumEnter` без своей привязки — это `Enter`, `GrayMinus` — `Minus`.
        self.fallback = (code != base).then(|| Stroke::new(base, mods));
        let mut pass = !self.feed(Stroke::new(code, mods), repeat, st, now, out, None);
        self.fallback = None;
        if self.capture {
            self.swallow = Some(key);
        }
        // Терминал (ADR-0015): без своей привязки — в PTY. Печатное приходит текстом
        // (раскладка, AltGr), сюда — непечатные клавиши и Ctrl/Alt+клавиша.
        if pass && st.focus == Focus::Terminal && self.km.has_command("term.key") {
            let ctrl = mods.contains(Mods::CTRL);
            let alt = mods.contains(Mods::ALT);
            if !prints(key) || ctrl != alt {
                let mut m = Vec::new();
                for (f, n) in [
                    (Mods::CTRL, "ctrl"),
                    (Mods::ALT, "alt"),
                    (Mods::SHIFT, "shift"),
                ] {
                    if mods.contains(f) {
                        m.push(n);
                    }
                }
                // Имя кодировщика: знак для клавиш-знаков (`[`), иначе имя (`up`, `f5`).
                let sym = key.symbol_or_name();
                let name = if sym.len() == 1 { sym } else { key.name() };
                let mut c = Command::new("term.key")
                    .arg("key", name.to_ascii_lowercase())
                    .from(Origin::Key);
                if !m.is_empty() {
                    c = c.arg("mods", m.join("+"));
                }
                out.push(c);
                self.suppress_text = true;
                pass = false;
            }
        }
        match i {
            Some(i) => self.down[i].passed |= pass,
            None => self.down.push(Down { key, passed: pass }),
        }
        pass
    }

    fn release(&mut self, key: Key) -> bool {
        if self.swallow == Some(key) {
            self.swallow = None;
        }
        match self.down.iter().position(|d| d.key == key) {
            Some(i) => self.down.swap_remove(i).passed,
            None => true,
        }
    }

    /// Клавиша события, уточнённая запиской хука, и она же без уточнения.
    fn code(&mut self, key: Key, physical: Option<Key>) -> (KeyCode, KeyCode) {
        if physical == Some(Key::Plus) {
            self.pair(vk::ADD);
            return (KeyCode::GrayPlus, KeyCode::Key(key));
        }
        let key = match (self.r#match, physical) {
            (MatchMode::Physical, Some(p)) => p,
            // Shift+цифра даёт символ (`!`, `№`): сочетание — по цифре (S3 §1.1 №2).
            (MatchMode::Auto, Some(p)) if is_digit(p) && !is_digit(key) => p,
            _ => key,
        };
        let own = match key {
            Key::Minus => (vk::SUBTRACT, KeyCode::GrayMinus),
            Key::Slash => (vk::DIVIDE, KeyCode::GraySlash),
            Key::Num5 => (vk::CLEAR, KeyCode::Num5),
            Key::Enter => (vk::RETURN, KeyCode::NumEnter),
            Key::Insert => (vk::INSERT, KeyCode::Key(key)),
            Key::Delete => (vk::DELETE, KeyCode::Key(key)),
            _ => return (KeyCode::Key(key), KeyCode::Key(key)),
        };
        let found = if own.0 == vk::RETURN {
            self.pair_with(|n| n.vk == vk::RETURN && n.extended)
        } else {
            self.pair(own.0)
        };
        if found.is_some() {
            (own.1, KeyCode::Key(key))
        } else {
            (KeyCode::Key(key), KeyCode::Key(key))
        }
    }

    fn pair(&mut self, vk: u16) -> Option<KeyNote> {
        self.pair_with(|n| n.vk == vk)
    }

    /// Первая неиспользованная записка нажатия, подходящая под `f`.
    fn pair_with(&mut self, f: impl Fn(&KeyNote) -> bool) -> Option<KeyNote> {
        let (n, used) = self
            .frame_notes
            .iter_mut()
            .find(|(n, used)| !*used && f(n))?;
        *used = true;
        Some(*n)
    }

    fn text(&mut self, t: &str, st: &AppState, now: Instant, out: &mut Vec<Command>) -> bool {
        if self.suppress_text || (self.capture && t != "*") {
            return false;
        }
        // Серая `*`: у egui нет `Key`, только `Text` — сочетание по записке.
        if t == "*"
            && let Some(n) = self.pair(vk::MULTIPLY)
            && self.feed(
                Stroke::new(KeyCode::GrayStar, n.mods),
                false,
                st,
                now,
                out,
                None,
            )
        {
            return false;
        }
        let id = match st.focus {
            Focus::Overlay(OverlayKind::QuickSearch) => "keymap.qs.input",
            Focus::Panel
                if self.qs_mode == QsMode::Alt
                    && self.mods.held.contains(Mods::ALT)
                    && !self.mods.held.intersects(Mods::CTRL | Mods::CMD) =>
            {
                "keymap.qs.start"
            }
            Focus::Panel | Focus::Cmdline => "cmdline.insert",
            Focus::Terminal => "term.input",
            _ => return true,
        };
        if !self.km.has_command(id) {
            return true;
        }
        out.push(Command::new(id).arg("text", t).from(Origin::Key));
        false
    }

    /// Lookup сочетания. `true` — поглощено (команда, начало или сброс аккорда, повтор
    /// без автоповтора).
    fn feed(
        &mut self,
        stroke: Stroke,
        repeat: bool,
        st: &AppState,
        now: Instant,
        out: &mut Vec<Command>,
        text: Option<&str>,
    ) -> bool {
        // Повтор при начатом аккорде ничего не меняет (S3 §1.1 №11).
        if repeat && self.pending.is_some() {
            return true;
        }
        if self.capture {
            self.suppress_text = true;
            if !repeat {
                self.captured = Some(crate::compile::generic(stroke));
            }
            return true;
        }
        let pending = self.pending.as_ref().map(|(c, _)| c);
        let km = Arc::clone(&self.km);
        let mut found = km.lookup(pending, stroke, st);
        if matches!(found, Lookup::Miss)
            && let Some(f) = self.fallback
        {
            found = km.lookup(pending, f, st);
        }
        match found {
            Lookup::Hit(cmds, repeatable) => {
                self.suppress_text = true;
                if repeat && !repeatable {
                    return true;
                }
                self.pending = None;
                emit(&self.km, cmds, text, out);
                true
            }
            Lookup::Prefix => {
                if !repeat {
                    let mut chord = pending.cloned().unwrap_or_default();
                    chord.strokes.push(stroke);
                    self.pending = Some((chord, now + self.chord_timeout));
                }
                true
            }
            Lookup::Miss if self.pending.take().is_some() => true,
            // Быстрый поиск: непривязанная клавиша (не печать) закрывает его и
            // выполняется как на панели (Far, S3 §1.3).
            Lookup::Miss if is_qs(st) && !texty(stroke) => {
                if self.km.has_command("keymap.qs.cancel") {
                    out.push(Command::new("keymap.qs.cancel").from(Origin::Key));
                }
                let as_panel = |s| km.lookup_as(None, s, st, Focus::Panel);
                let mut again = as_panel(stroke);
                if matches!(again, Lookup::Miss)
                    && let Some(f) = self.fallback
                {
                    again = as_panel(f);
                }
                if let Lookup::Hit(cmds, _) = again {
                    self.suppress_text = true;
                    emit(&km, cmds, text, out);
                }
                true
            }
            Lookup::Miss => false,
        }
    }

    /// Одиночный Alt/Ctrl (S3 §1.4): нажат один модификатор и отпущен за `alt_tap_ms`,
    /// между ними не было клавиш, кнопок мыши, колеса и других модификаторов.
    fn modifiers_changed(
        &mut self,
        new: Mods,
        st: &AppState,
        now: Instant,
        out: &mut Vec<Command>,
    ) {
        if self.capture {
            self.tap = None;
            return;
        }
        let prev = self.mods.held;
        self.tap = match self.tap {
            Some((k, t0)) if new.is_empty() && prev == k => {
                if now.duration_since(t0) <= self.alt_tap && !self.ime {
                    self.fire_tap(k, st, now, out);
                }
                None
            }
            _ if prev.is_empty() && (new == Mods::ALT || new == Mods::CTRL) => Some((new, now)),
            _ => None,
        };
    }

    /// `AltTap`/`CtrlTap`: привязка, иначе — начало быстрого поиска в режиме
    /// `alt_tap`/`ctrl_tap`. В самом поиске одиночный модификатор ничего не делает.
    fn fire_tap(&mut self, kind: Mods, st: &AppState, now: Instant, out: &mut Vec<Command>) {
        if is_qs(st) {
            return;
        }
        let (code, mode) = if kind == Mods::ALT {
            (KeyCode::AltTap, QsMode::AltTap)
        } else {
            (KeyCode::CtrlTap, QsMode::CtrlTap)
        };
        if self.feed(Stroke::new(code, Mods::empty()), false, st, now, out, None) {
            return;
        }
        if self.qs_mode == mode
            && st.focus == Focus::Panel
            && self.km.has_command("keymap.qs.start")
        {
            out.push(Command::new("keymap.qs.start").from(Origin::Key));
        }
    }

    /// Сообщить handler `keymap` о начатом или сброшенном аккорде (строка статуса).
    fn sync_pending(&mut self, out: &mut Vec<Command>) {
        let shown = self
            .pending
            .as_ref()
            .map(|(c, _)| Chord(c.strokes.clone()).display(self.os));
        if shown == self.shown_pending {
            return;
        }
        if self.km.has_command("keymap.pending") {
            let mut c = Command::new("keymap.pending").from(Origin::Key);
            if let Some(s) = &shown {
                c = c.arg("chord", s.as_str());
            }
            out.push(c);
        }
        self.shown_pending = shown;
    }

    /// Записка без события egui (`Num5` без `NumLock`, `Shift+Ins` при пустом буфере,
    /// `Ctrl+GrayStar`) — синтетическое сочетание.
    fn unpaired_notes(
        &mut self,
        raw: &mut crate::ev::RawInput,
        st: &AppState,
        now: Instant,
        out: &mut Vec<Command>,
    ) {
        if self.ime {
            return;
        }
        let mut i = 0;
        while i < self.frame_notes.len() {
            let (n, used) = self.frame_notes[i];
            i += 1;
            if used {
                continue;
            }
            let (code, ch) = match n.vk {
                vk::CLEAR => (KeyCode::Num5, None),
                vk::MULTIPLY => (KeyCode::GrayStar, Some("*")),
                vk::ADD => (KeyCode::GrayPlus, Some("+")),
                vk::SUBTRACT => (KeyCode::GrayMinus, Some("-")),
                vk::DIVIDE => (KeyCode::GraySlash, Some("/")),
                vk::RETURN if n.extended => (KeyCode::NumEnter, None),
                vk::INSERT => (KeyCode::Key(Key::Insert), None),
                vk::DELETE => (KeyCode::Key(Key::Delete), None),
                _ => continue,
            };
            if !self.feed(Stroke::new(code, n.mods), false, st, now, out, None)
                && let Some(ch) = ch
                && !n.mods.intersects(Mods::CTRL | Mods::ALT | Mods::CMD)
                && matches!(st.focus, Focus::Cmdline | Focus::Editor)
            {
                raw.events.push(Event::Text(ch.into()));
            }
        }
    }
}

/// Команды привязки с `Origin::Key`; текст вставки — в `text`, если команда его берёт.
fn emit(km: &Keymap, cmds: &[Command], text: Option<&str>, out: &mut Vec<Command>) {
    for c in cmds {
        let mut c = c.clone().from(Origin::Key);
        if let Some(t) = text
            && c.args.get("text").is_none()
            && km.takes_text(c.id.as_str())
        {
            c = c.arg("text", t);
        }
        out.push(c);
    }
}

fn is_qs(st: &AppState) -> bool {
    st.focus == Focus::Overlay(OverlayKind::QuickSearch)
}

/// Нажатие печатает символ (за ним придёт `Text`): без Ctrl/Cmd, клавиша символа,
/// пробел или серые `+-*/`. Alt допускается — Far продолжает поиск по Alt+буква.
fn texty(s: Stroke) -> bool {
    if s.mods
        .intersects(Mods::CTRL | Mods::LCTRL | Mods::RCTRL | Mods::CMD)
    {
        return false;
    }
    match s.key {
        KeyCode::GrayPlus | KeyCode::GrayMinus | KeyCode::GrayStar | KeyCode::GraySlash => true,
        KeyCode::Key(k) => {
            k == Key::Space
                || k.name().chars().count() == 1
                || matches!(
                    k,
                    Key::Minus
                        | Key::Plus
                        | Key::Equals
                        | Key::OpenBracket
                        | Key::CloseBracket
                        | Key::Backslash
                        | Key::Semicolon
                        | Key::Quote
                        | Key::Comma
                        | Key::Period
                        | Key::Slash
                        | Key::Backtick
                        | Key::Colon
                        | Key::Pipe
                        | Key::Questionmark
                        | Key::Exclamationmark
                        | Key::OpenCurlyBracket
                        | Key::CloseCurlyBracket
                )
        }
        _ => false,
    }
}

fn is_digit(k: Key) -> bool {
    matches!(
        k,
        Key::Num0
            | Key::Num1
            | Key::Num2
            | Key::Num3
            | Key::Num4
            | Key::Num5
            | Key::Num6
            | Key::Num7
            | Key::Num8
            | Key::Num9
    )
}

#[cfg(test)]
#[path = "input_tests.rs"]
mod tests;
