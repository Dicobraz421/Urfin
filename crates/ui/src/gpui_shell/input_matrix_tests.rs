//! Матрица раскладок (S3-11, G-03; v4 №5): сочетания с буквой и символом дают одну и ту
//! же команду в раскладках en-US, ru, uk, el, he, ar, ja (IME вкл/выкл), zh-Pinyin, ko,
//! Dvorak, AZERTY, QWERTZ.
//!
//! Ввод — синтетический: модель того, что окно gpui на Windows (`pets-gpui-windows`,
//! `events.rs`/`keyboard.rs`) отдаёт мосту на каждое нажатие — `Keystroke` (`key`,
//! `key_char`, модификаторы) и [`KeyMsg`] subclass (скан-код, Shift, надпись). Дальше —
//! настоящий мост ([`Input`]) и конвейер keymap. Раскладку ОС тесты не трогают.
//!
//! Что моделируется из gpui:
//! - `key` — `MapVirtualKeyW(vk, MAPVK_VK_TO_CHAR)` в нижнем регистре ASCII: у букв
//!   (`VK_A…VK_Z`) латиница при любой раскладке (проверено на стенде для en и ru), у
//!   прочих клавиш — символ раскладки (`х` на месте `[` в русской);
//! - Shift на цифре или символе: `key` — символ со Shift, Shift в модификаторах снят;
//! - `key_char` — `ToUnicode` с модификаторами: при Ctrl — управляющий символ или ничего
//!   (gpui отбрасывает), при Alt — символ раскладки;
//! - IME включён: клавишу без Ctrl/Alt IME забирает (`VK_PROCESSKEY`), нажатия gpui нет;
//!   отпускание приходит с настоящим кодом.
//!
//! Таблицы раскладок — символы клавиш без Shift и со Shift по местам US-QWERTY. Буквы,
//! скобки и цифры сверены по раскладкам Windows; прочие символы на ход матрицы не влияют.

use gpui_kit::{Keystroke, Modifiers};
use urfin_core::{
    AppState, ArgSpec, Command, CommandMeta, CommandRegistry, Focus, Location, Panel, Tab, TabId,
    ValueKind,
};
use urfin_keymap::{FlagRegistry, KeyInput, Keymap, KeymapCfg, Layer, MatchMode, Os};

use super::{Input, KeyMsg};

/// Места основного блока US-QWERTY по порядку строк таблиц: `` ` ``, цифровой ряд,
/// верхний ряд, `\`, средний ряд, нижний — 47 клавиш.
const US: &str = "`1234567890-=qwertyuiop[]\\asdfghjkl;'zxcvbnm,./";
const US_SHIFT: &str = "~!@#$%^&*()_+QWERTYUIOP{}|ASDFGHJKL:\"ZXCVBNM<>?";

/// Скан-коды мест [`US`].
fn scans() -> Vec<u8> {
    let mut s = vec![0x29];
    s.extend(0x02..=0x0D);
    s.extend(0x10..=0x1B);
    s.push(0x2B);
    s.extend(0x1E..=0x28);
    s.extend(0x2C..=0x35);
    s
}

/// Нет символа.
const NONE: char = '∅';

struct Layout {
    name: &'static str,
    base: &'static str,
    shift: &'static str,
    /// Коды букв по надписи (латинские раскладки: `VK_C` — на клавише `c` Dvorak) или по
    /// месту US-QWERTY (кириллица, греческий, иврит, арабский).
    latin: bool,
    /// IME включён.
    ime: bool,
}

const fn latin(name: &'static str, base: &'static str, shift: &'static str) -> Layout {
    Layout {
        name,
        base,
        shift,
        latin: true,
        ime: false,
    }
}

const fn other(name: &'static str, base: &'static str, shift: &'static str) -> Layout {
    Layout {
        name,
        base,
        shift,
        latin: false,
        ime: false,
    }
}

const fn ime(mut l: Layout) -> Layout {
    l.ime = true;
    l
}

/// Японская 106: `半角/全角` на месте `` ` ``, `^ @ [ ] : ` — на своих местах.
const JA: (&str, &str) = (
    "∅1234567890-^qwertyuiop@[]asdfghjkl;:zxcvbnm,./",
    "∅!\"#$%&'()∅=~QWERTYUIOP`{}ASDFGHJKL+*ZXCVBNM<>?",
);

const LAYOUTS: [Layout; 13] = [
    latin("en-US", US, US_SHIFT),
    other(
        "ru",
        "ё1234567890-=йцукенгшщзхъ\\фывапролджэячсмитьбю.",
        "Ё!\"№;%:?*()_+ЙЦУКЕНГШЩЗХЪ/ФЫВАПРОЛДЖЭЯЧСМИТЬБЮ,",
    ),
    other(
        "uk",
        "'1234567890-=йцукенгшщзхїґфівапролджєячсмитьбю.",
        "₴!\"№;%:?*()_+ЙЦУКЕНГШЩЗХЇҐФІВАПРОЛДЖЄЯЧСМИТЬБЮ,",
    ),
    other(
        "el",
        "`1234567890-=;ςερτυθιοπ[]\\ασδφγηξκλ΄'ζχψωβνμ,./",
        "~!@#$%^&*()_+:΅ΕΡΤΥΘΙΟΠ{}|ΑΣΔΦΓΗΞΚΛ¨\"ΖΧΨΩΒΝΜ<>?",
    ),
    // Иврит: скобки зеркальные — `]` на месте `[`.
    other(
        "he",
        ";1234567890-=/'קראטוןםפ][\\שדגכעיחלךף,זסבהנמצתץ.",
        "~!@#$%^&*)(_+QWERTYUIOP}{|ASDFGHJKL:\"ZXCVBNM><?",
    ),
    // Арабская 101; `لا` — одной лигатурой.
    other(
        "ar",
        "ذ1234567890-=ضصثقفغعهخحجد\\شسيبلاتنمكطئءؤر\u{fefb}ىةوزظ",
        "\u{651}!@#$%^&*)(_+\u{64e}\u{64b}\u{64f}\u{64c}\u{fef9}\u{625}\u{2018}\u{f7}\u{d7}\u{61b}<>|\u{650}\u{64d}][\u{fef7}\u{623}\u{640}\u{60c}/:\"~\u{652}}{\u{fef5}\u{622}\u{2019},.\u{61f}",
    ),
    latin("ja (IME выкл)", JA.0, JA.1),
    ime(latin("ja (IME вкл)", JA.0, JA.1)),
    ime(latin("zh-Pinyin (IME вкл)", US, US_SHIFT)),
    ime(latin("ko (IME вкл)", US, US_SHIFT)),
    latin(
        "Dvorak",
        "`1234567890[]',.pyfgcrl/=\\aoeuidhtns-;qjkxbmwvz",
        "~!@#$%^&*(){}\"<>PYFGCRL?+|AOEUIDHTNS_:QJKXBMWVZ",
    ),
    // `^` — мёртвая клавиша.
    latin(
        "AZERTY",
        "²&é\"'(-è_çà)=azertyuiop^$*qsdfghjklmùwxcvbn,;:!",
        "∅1234567890°+AZERTYUIOP¨£µQSDFGHJKLM%WXCVBN?./§",
    ),
    // `^` и `´` — мёртвые клавиши.
    latin(
        "QWERTZ",
        "^1234567890ß´qwertzuiopü+#asdfghjklöäyxcvbnm,.-",
        "°!\"§$%&/()=?`QWERTZUIOPÜ*'ASDFGHJKLÖÄYXCVBNM;:_",
    ),
];

fn at(row: &str, i: usize) -> Option<char> {
    row.chars().nth(i).filter(|c| *c != NONE)
}

/// Клавиша, которую нажимают для сочетания с символом `t` (v4 №5): с этой надписью в
/// раскладке; такой нет — на месте `t` в US-QWERTY.
fn key_for(l: &Layout, t: char) -> usize {
    l.base
        .chars()
        .position(|c| c == t)
        .or_else(|| US.chars().position(|c| c == t))
        .unwrap_or_else(|| panic!("нет клавиши {t}"))
}

/// `VK_OEM_*`: для моста неважно, какой именно (важно — не `VK_ADD`).
const VK_OEM: u16 = 0xBA;

/// Что gpui отдаёт на нажатие (`down`) или отпускание клавиши `i` раскладки `l`.
fn gpui(l: &Layout, i: usize, m: Modifiers, down: bool) -> Option<(Keystroke, KeyMsg)> {
    let base = at(l.base, i);
    let shifted = at(l.shift, i);
    let us = at(US, i)?;
    let letter = if l.latin { base } else { Some(us) }.filter(char::is_ascii_lowercase);
    let vk = match letter {
        Some(c) => u16::from(c.to_ascii_uppercase() as u8),
        // Цифровой ряд — `VK_0…VK_9` во всех раскладках матрицы (и в AZERTY).
        None if us.is_ascii_digit() => u16::from(us as u8),
        None => VK_OEM,
    };
    if l.ime && down && !m.control && !m.alt {
        return None;
    }
    let label = letter.map(|c| c.to_ascii_uppercase()).or(base);
    let mut mods = m;
    let key = if m.shift && letter.is_none() {
        mods.shift = false;
        shifted?.to_string()
    } else {
        label?.to_ascii_lowercase().to_string()
    };
    let key_char = if m.control {
        None
    } else if m.shift {
        shifted
    } else {
        base
    };
    let ks = Keystroke {
        modifiers: mods,
        key,
        key_char: key_char.map(String::from),
    };
    let msg = KeyMsg {
        vk,
        scan: scans()[i],
        extended: false,
        shift: m.shift,
        label,
    };
    Some((ks, msg))
}

/// Alt+F7: F-клавиши gpui называет сами.
fn f7(m: Modifiers) -> (Keystroke, KeyMsg) {
    let ks = Keystroke {
        modifiers: m,
        key: "f7".into(),
        key_char: None,
    };
    let msg = KeyMsg {
        vk: 0x76,
        scan: 0x41,
        extended: false,
        shift: m.shift,
        label: None,
    };
    (ks, msg)
}

const TEXT: &[ArgSpec] = &[ArgSpec {
    name: "text",
    kind: ValueKind::Str,
    required: true,
}];

const fn meta(id: &'static str, args: &'static [ArgSpec]) -> CommandMeta {
    CommandMeta {
        id,
        title: id,
        category: "test",
        args,
        repeatable: false,
    }
}

static METAS: &[CommandMeta] = &[
    meta("mx.c", &[]),
    meta("mx.v", &[]),
    meta("mx.a", &[]),
    meta("mx.q", &[]),
    meta("mx.u", &[]),
    meta("mx.o", &[]),
    meta("mx.r", &[]),
    meta("mx.alt_f7", &[]),
    meta("mx.lbr", &[]),
    meta("mx.rbr", &[]),
    meta("mx.shift_lbr", &[]),
    meta("mx.one", &[]),
    meta("mx.shift_one", &[]),
    meta("cmdline.insert", TEXT),
    meta("keymap.qs.start", TEXT),
];

/// Привязки матрицы — как в пресетах Far/DN/base (`Ctrl+OpenBracket`, `Ctrl+Shift+1`…).
const PRESET: &str = r#"format = 1
name = "matrix"
[context.panel]
"Ctrl+C" = "mx.c"
"Ctrl+V" = "mx.v"
"Ctrl+A" = "mx.a"
"Ctrl+Q" = "mx.q"
"Ctrl+U" = "mx.u"
"Ctrl+O" = "mx.o"
"Ctrl+R" = "mx.r"
"Alt+F7" = "mx.alt_f7"
"Ctrl+OpenBracket" = "mx.lbr"
"Ctrl+CloseBracket" = "mx.rbr"
"Ctrl+Shift+OpenBracket" = "mx.shift_lbr"
"Ctrl+1" = "mx.one"
"Ctrl+Shift+1" = "mx.shift_one"
"#;

fn bridge(cfg: &KeymapCfg) -> Input {
    let mut reg = CommandRegistry::new();
    reg.register(METAS).unwrap();
    let (km, w) = Keymap::from_str_layers(
        &[(PRESET, Layer::Preset)],
        &reg,
        &FlagRegistry::with_builtin(),
        Os::Windows,
    );
    assert_eq!(w, Vec::new());
    Input::new(KeyInput::new(km, cfg))
}

fn panel() -> AppState {
    let loc = Location::local(r"C:\w");
    let mut st = AppState::new(
        Panel::new(Tab::new(TabId(1), loc.clone())),
        Panel::new(Tab::new(TabId(2), loc)),
    );
    st.focus = Focus::Panel;
    st
}

/// Клавиша сочетания.
#[derive(Copy, Clone, Debug)]
enum Hit {
    /// Клавиша с этим символом (v4 №5, [`key_for`]).
    Char(char),
    F7,
}

/// Команда сочетания: id и `text`.
type Got = Vec<(String, Option<String>)>;

/// Сочетание, как его видит мост: модификаторы, нажатие, отпускание, модификаторы.
fn press(input: &mut Input, st: &AppState, l: &Layout, hit: Hit, m: Modifiers) -> Got {
    let mut out: Vec<Command> = Vec::new();
    let held = m != Modifiers::default();
    if held {
        input.modifiers(m, st, &mut out);
    }
    let (down, up) = match hit {
        Hit::Char(t) => {
            let i = key_for(l, t);
            (gpui(l, i, m, true), gpui(l, i, m, false))
        }
        Hit::F7 => (Some(f7(m)), Some(f7(m))),
    };
    if let Some((ks, msg)) = down {
        input.key_down_with(&ks, Some(msg), st, &mut out);
    }
    if let Some((ks, msg)) = up {
        input.key_up_with(&ks, Some(msg), st, &mut out);
    }
    if held {
        input.modifiers(Modifiers::default(), st, &mut out);
    }
    out.iter()
        .map(|c| {
            (
                c.id.as_str().to_owned(),
                c.args.str("text").map(str::to_owned),
            )
        })
        .collect()
}

fn ctrl() -> Modifiers {
    Modifiers {
        control: true,
        ..Modifiers::default()
    }
}

fn ctrl_shift() -> Modifiers {
    Modifiers {
        shift: true,
        ..ctrl()
    }
}

fn alt() -> Modifiers {
    Modifiers {
        alt: true,
        ..Modifiers::default()
    }
}

/// Сочетания матрицы: имя, клавиша, модификаторы, команда.
fn cases() -> Vec<(&'static str, Hit, Modifiers, &'static str)> {
    let mut v = Vec::new();
    for (t, id) in [
        ('c', "mx.c"),
        ('v', "mx.v"),
        ('a', "mx.a"),
        ('q', "mx.q"),
        ('u', "mx.u"),
        ('o', "mx.o"),
        ('r', "mx.r"),
    ] {
        v.push(("Ctrl+буква", Hit::Char(t), ctrl(), id));
    }
    v.extend([
        ("Alt+F7", Hit::F7, alt(), "mx.alt_f7"),
        ("Ctrl+[", Hit::Char('['), ctrl(), "mx.lbr"),
        ("Ctrl+]", Hit::Char(']'), ctrl(), "mx.rbr"),
        ("Ctrl+Shift+[", Hit::Char('['), ctrl_shift(), "mx.shift_lbr"),
        ("Ctrl+1", Hit::Char('1'), ctrl(), "mx.one"),
        ("Ctrl+Shift+1", Hit::Char('1'), ctrl_shift(), "mx.shift_one"),
    ]);
    v
}

/// Сочетания, недостижимые по правилу v4 №5 как оно записано: в QWERTZ `]` есть только
/// с `AltGr`, а на месте `]` US — `+`, латинский символ, и сочетание идёт по нему
/// (`Ctrl+Plus`). Решение — у владельца (ADR-0019, предложение). Закроется — убрать.
const GAPS: [(&str, char); 1] = [("QWERTZ", ']')];

fn gap(l: &Layout, hit: Hit) -> bool {
    matches!(hit, Hit::Char(t) if GAPS.contains(&(l.name, t)))
}

/// Каждое сочетание матрицы в каждой раскладке даёт свою команду — и при повторном
/// нажатии (отпускание опознано той же клавишей, повтором нажатие не считается).
#[test]
fn layout_matrix() {
    let st = panel();
    let mut failed = Vec::new();
    for l in &LAYOUTS {
        assert_eq!(l.base.chars().count(), 47, "{}: без Shift", l.name);
        assert_eq!(l.shift.chars().count(), 47, "{}: со Shift", l.name);
        let mut input = bridge(&KeymapCfg::default());
        for (name, hit, m, id) in cases() {
            for round in 0..2 {
                let got = press(&mut input, &st, l, hit, m);
                let ok = got == [(id.to_owned(), None)];
                if gap(l, hit) {
                    assert!(!ok, "{}: {name} уже работает — убрать из GAPS", l.name);
                } else if !ok {
                    failed.push(format!("{} {name} (#{round}): {got:?}, ждали {id}", l.name));
                }
            }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// Alt+буква — быстрый поиск Far с набранным символом раскладки (v4 №5: `Text`, не
/// привязка); при IME тоже — Alt IME не забирает.
#[test]
fn alt_letter_quick_search() {
    let st = panel();
    for l in &LAYOUTS {
        let mut input = bridge(&KeymapCfg::default());
        let i = key_for(l, 'f');
        let typed = at(l.base, i).unwrap().to_string();
        let got = press(&mut input, &st, l, Hit::Char('f'), alt());
        assert_eq!(
            got,
            [("keymap.qs.start".to_owned(), Some(typed))],
            "{}",
            l.name
        );
    }
}

/// `match = "physical"`: Dvorak — по месту US-QWERTY (на месте `C` — `j`); `auto` — по
/// надписи (`Ctrl+J` не привязан).
#[test]
fn physical_match_on_dvorak() {
    let st = panel();
    let dvorak = LAYOUTS.iter().find(|l| l.name == "Dvorak").unwrap();
    let i = US.chars().position(|c| c == 'c').unwrap();
    let hit = |cfg: &KeymapCfg| {
        let mut input = bridge(cfg);
        let mut out = Vec::new();
        input.modifiers(ctrl(), &st, &mut out);
        let (ks, msg) = gpui(dvorak, i, ctrl(), true).unwrap();
        assert_eq!(ks.key, "j");
        input.key_down_with(&ks, Some(msg), &st, &mut out);
        out.iter().map(|c| c.id.to_string()).collect::<Vec<_>>()
    };
    let physical = KeymapCfg {
        r#match: MatchMode::Physical,
        ..KeymapCfg::default()
    };
    assert_eq!(hit(&physical), ["mx.c"]);
    assert_eq!(hit(&KeymapCfg::default()), Vec::<String>::new());
}

/// IME включён: буква без модификаторов не доходит до keymap (её набирает IME), её
/// отпускание без нажатия ничего не ломает — следующий Ctrl+C работает.
#[test]
fn ime_letter_then_hotkey() {
    let st = panel();
    let ja = LAYOUTS.iter().find(|l| l.name == "ja (IME вкл)").unwrap();
    let mut input = bridge(&KeymapCfg::default());
    let none = Modifiers::default();
    assert_eq!(press(&mut input, &st, ja, Hit::Char('c'), none), Got::new());
    assert_eq!(
        press(&mut input, &st, ja, Hit::Char('c'), ctrl()),
        [("mx.c".to_owned(), None)]
    );
}

/// Модель сверена с раскладками стенда (en, ru: `MapVirtualKeyExW`): буквы — латиница,
/// на месте `[` русской — `х`, Shift+`[` — `Х`; gpui снимает Shift.
#[test]
fn model_matches_stand() {
    let ru = &LAYOUTS[1];
    let (ks, msg) = gpui(ru, key_for(ru, 'c'), ctrl(), true).unwrap();
    assert_eq!((ks.key.as_str(), ks.key_char.as_deref()), ("c", None));
    assert_eq!((msg.vk, msg.scan, msg.label), (0x43, 0x2E, Some('C')));
    let (ks, msg) = gpui(ru, key_for(ru, '['), ctrl_shift(), true).unwrap();
    assert_eq!((ks.key.as_str(), ks.modifiers.shift), ("Х", false));
    assert_eq!((msg.scan, msg.shift, msg.label), (0x1A, true, Some('х')));
    let (ks, _) = gpui(ru, key_for(ru, 'f'), alt(), true).unwrap();
    assert_eq!((ks.key.as_str(), ks.key_char.as_deref()), ("f", Some("а")));
}

/// Захват сочетания (раздел «Клавиатура») на любой раскладке — та же клавиша keymap, что
/// срабатывает в [`layout_matrix`]: буква по надписи (AZERTY) или по месту (кириллица).
#[test]
fn capture_layout_independent() {
    let mut st = panel();
    crate::keys_settings::tests::start_capture(&mut st);
    for l in &LAYOUTS {
        let mut input = bridge(&KeymapCfg::default());
        for (hit, want) in [(Hit::Char('a'), "Ctrl+A"), (Hit::Char('q'), "Ctrl+Q")] {
            if gap(l, hit) {
                continue;
            }
            let got = chord_of(&mut input, &st, l, hit, ctrl());
            assert_eq!(got.as_deref(), Some(want), "{}", l.name);
        }
        let got = chord_of(&mut input, &st, l, Hit::F7, alt());
        assert_eq!(got.as_deref(), Some("Alt+F7"), "{}", l.name);
    }
}

/// Что записал захват: команды, кроме `ui.keys`, — ошибка.
fn chord_of(input: &mut Input, st: &AppState, l: &Layout, hit: Hit, m: Modifiers) -> Option<String> {
    let mut out: Vec<Command> = Vec::new();
    input.modifiers(m, st, &mut out);
    let (down, up) = match hit {
        Hit::Char(t) => {
            let i = key_for(l, t);
            (gpui(l, i, m, true), gpui(l, i, m, false))
        }
        Hit::F7 => (Some(f7(m)), Some(f7(m))),
    };
    if let Some((ks, msg)) = down {
        input.key_down_with(&ks, Some(msg), st, &mut out);
    }
    if let Some((ks, msg)) = up {
        input.key_up_with(&ks, Some(msg), st, &mut out);
    }
    input.modifiers(Modifiers::default(), st, &mut out);
    assert!(out.iter().all(|c| c.id.as_str() == "ui.keys"), "{out:?}");
    out.first().and_then(|c| c.args.str("chord")).map(str::to_owned)
}
