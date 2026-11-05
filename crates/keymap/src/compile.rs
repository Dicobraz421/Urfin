//! Компиляция keymap и поиск (S3-04): одиночные сочетания — хэш-таблица, аккорды —
//! таблица полных аккордов и префиксов. Ключи до трёх нажатий — без кучи (`lookup` —
//! 0 аллокаций, <300 нс).
//!
//! Победитель среди сработавших привязок: специфичность сработавшей конъюнкции, затем
//! слой (`user > os > preset > base`), затем порядок загрузки (последняя строка).
//! Привязка со стороной модификатора (`LCtrl`) точнее общей (`Ctrl`).

use std::sync::Arc;

use compact_str::CompactString;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use urfin_core::{AppState, Command, Focus};

use crate::key::{Chord, KeyCode, Mods, Stroke};
use crate::keymap::{Binding, Layer};
use crate::when::When;

/// Нажатия аккорда (до 3, S3 §2.2).
pub type Strokes = SmallVec<[Stroke; 3]>;

/// Запись таблицы.
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub when: Arc<When>,
    pub cmds: Arc<[Command]>,
    pub layer: Layer,
    pub order: u32,
    pub repeatable: bool,
}

/// Результат поиска.
#[derive(Debug, PartialEq)]
pub enum Lookup<'a> {
    /// Команды и разрешён ли автоповтор при удержании (`_repeat`, иначе
    /// `CommandMeta::repeatable`).
    Hit(&'a [Command], bool),
    /// Начало аккорда: ждать следующее нажатие.
    Prefix,
    Miss,
}

/// Начатый аккорд (ведёт `KeyInput`, S3-05).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChordState {
    pub strokes: Strokes,
}

/// Подпись F-клавиши для keybar S2: команда (с аргументами) и ключ Fluent.
#[derive(Clone, Debug, PartialEq)]
pub struct KeybarItem {
    pub cmd: Command,
    /// `keybar-<id>[-<значение>]`: `keybar-panel-sort-name`, `keybar-ops-copy-current`.
    pub label_key: CompactString,
}

/// Привязка в `keymap.show`/`keymap.dump`.
#[derive(Debug)]
pub struct EffectiveBinding<'a> {
    pub chord: &'a Chord,
    pub binding: &'a Binding,
}

#[derive(Debug, Default)]
pub(crate) struct Compiled {
    single: FxHashMap<Stroke, SmallVec<[Entry; 2]>>,
    chords: FxHashMap<Strokes, SmallVec<[Entry; 2]>>,
    /// Префиксы аккордов и контексты аккордов под ними.
    prefixes: FxHashMap<Strokes, SmallVec<[Arc<When>; 2]>>,
}

impl Compiled {
    pub(crate) fn build(bindings: &[Binding]) -> Self {
        let mut c = Compiled::default();
        for (i, b) in bindings.iter().enumerate() {
            let e = Entry {
                when: Arc::clone(&b.when),
                cmds: Arc::clone(&b.cmds),
                layer: b.layer,
                order: u32::try_from(i).unwrap_or(u32::MAX),
                repeatable: b.repeatable,
            };
            let strokes = &b.chord.0;
            if strokes.len() == 1 {
                c.single.entry(strokes[0]).or_default().push(e);
            } else {
                for n in 1..strokes.len() {
                    let p: Strokes = strokes[..n].iter().copied().collect();
                    let list = c.prefixes.entry(p).or_default();
                    if !list.iter().any(|w| Arc::ptr_eq(w, &b.when)) {
                        list.push(Arc::clone(&b.when));
                    }
                }
                c.chords.entry(strokes.clone()).or_default().push(e);
            }
        }
        c
    }

    /// Поиск нажатия `s` после начатого аккорда `pending`.
    /// Контексты — при фокусе `focus` (обычно `st.focus`).
    pub(crate) fn lookup(
        &self,
        pending: Option<&ChordState>,
        s: Stroke,
        st: &AppState,
        focus: Focus,
    ) -> Lookup<'_> {
        for v in variants(s).into_iter().flatten() {
            let found = match pending.filter(|p| !p.strokes.is_empty()) {
                None => self.find_single(v, st, focus),
                Some(p) => self.find_chord(p, v, st, focus),
            };
            if !matches!(found, Lookup::Miss) {
                return found;
            }
        }
        Lookup::Miss
    }

    fn find_single(&self, s: Stroke, st: &AppState, focus: Focus) -> Lookup<'_> {
        let one: Strokes = std::iter::once(s).collect();
        // Аккорд с этим началом в сработавшем контексте побеждает (PrefixShadow).
        if self.prefix_live(&one, st, focus) {
            return Lookup::Prefix;
        }
        match self.single.get(&s).and_then(|list| best(list, st, focus)) {
            Some(e) => Lookup::Hit(&e.cmds, e.repeatable),
            None => Lookup::Miss,
        }
    }

    fn find_chord(&self, p: &ChordState, s: Stroke, st: &AppState, focus: Focus) -> Lookup<'_> {
        let mut key = p.strokes.clone();
        key.push(s);
        if let Some(e) = self.chords.get(&key).and_then(|list| best(list, st, focus)) {
            return Lookup::Hit(&e.cmds, e.repeatable);
        }
        if self.prefix_live(&key, st, focus) {
            return Lookup::Prefix;
        }
        Lookup::Miss
    }

    fn prefix_live(&self, p: &Strokes, st: &AppState, focus: Focus) -> bool {
        self.prefixes
            .get(p)
            .is_some_and(|ws| ws.iter().any(|w| w.matches_as(st, focus).is_some()))
    }

    /// Сработает ли именно эта запись на своём аккорде в состоянии `st`.
    pub(crate) fn wins(&self, chord: &Chord, order: u32, st: &AppState) -> bool {
        let list = if chord.0.len() == 1 {
            if self.prefix_live(&chord.0, st, st.focus) {
                return false;
            }
            self.single.get(&chord.0[0])
        } else {
            self.chords.get(&chord.0)
        };
        list.and_then(|l| best(l, st, st.focus))
            .is_some_and(|e| e.order == order)
    }
}

/// Лучшая сработавшая запись.
fn best<'a>(list: &'a [Entry], st: &AppState, focus: Focus) -> Option<&'a Entry> {
    list.iter()
        .filter_map(|e| e.when.matches_as(st, focus).map(|spec| (spec, e)))
        .max_by_key(|(spec, e)| (*spec, e.layer, e.order))
        .map(|(_, e)| e)
}

/// Варианты нажатия для поиска: со сторонами модификаторов (если они есть), затем
/// общий (`LCtrl` → `Ctrl`).
fn variants(s: Stroke) -> [Option<Stroke>; 2] {
    let side_bits = Mods::LCTRL | Mods::RCTRL | Mods::LALT | Mods::RALT;
    let m = s.mods;
    let mut generic = m - side_bits;
    if m.intersects(Mods::LCTRL | Mods::RCTRL) {
        generic |= Mods::CTRL;
    }
    if m.intersects(Mods::LALT | Mods::RALT) {
        generic |= Mods::ALT;
    }
    let mut exact = m;
    if m.intersects(Mods::LCTRL | Mods::RCTRL) {
        exact -= Mods::CTRL;
    }
    if m.intersects(Mods::LALT | Mods::RALT) {
        exact -= Mods::ALT;
    }
    if exact == generic {
        [Some(Stroke::new(s.key, generic)), None]
    } else {
        [
            Some(Stroke::new(s.key, exact)),
            Some(Stroke::new(s.key, generic)),
        ]
    }
}

/// Сочетание без сторон модификаторов (`LCtrl+K` → `Ctrl+K`): так оно пишется в файл.
pub(crate) fn generic(s: Stroke) -> Stroke {
    let [first, second] = variants(s);
    second.or(first).unwrap_or(s)
}

/// Ключ подписи keybar: `keybar-<id>` и значения аргументов (`Str` — значение,
/// `true` — имя аргумента).
pub(crate) fn label_key(cmd: &Command) -> CompactString {
    let mut key = CompactString::from("keybar-");
    key.push_str(&cmd.id.as_str().replace('.', "-"));
    for (k, v) in cmd.args.iter() {
        match v {
            urfin_core::Value::Str(s) => {
                key.push('-');
                key.push_str(s);
            }
            urfin_core::Value::Bool(true) => {
                key.push('-');
                key.push_str(k);
            }
            _ => {}
        }
    }
    key
}

/// F1…F12.
pub(crate) const FKEYS: [crate::ev::Key; 12] = [
    crate::ev::Key::F1,
    crate::ev::Key::F2,
    crate::ev::Key::F3,
    crate::ev::Key::F4,
    crate::ev::Key::F5,
    crate::ev::Key::F6,
    crate::ev::Key::F7,
    crate::ev::Key::F8,
    crate::ev::Key::F9,
    crate::ev::Key::F10,
    crate::ev::Key::F11,
    crate::ev::Key::F12,
];

pub(crate) fn fkey_stroke(i: usize, mods: Mods) -> Stroke {
    Stroke::new(KeyCode::Key(FKEYS[i]), mods)
}

#[cfg(test)]
#[path = "compile_tests.rs"]
pub(crate) mod tests;
