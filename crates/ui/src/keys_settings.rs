//! Раздел настроек «Клавиатура» без gpui (`screen-settings-keys.png`, решение 2026-11-05):
//! курсор и поиск таблицы, захват сочетания, конфликт, пояснения, статус и клавиши окна.
//! Таблицу считает `urfin_keymap::Catalog`, файл пишет handler `keymap`; рисует
//! `gpui_shell::settings_keys`.

use std::sync::{Arc, Mutex, PoisonError};

use compact_str::CompactString;
use urfin_core::{Args, CmdError, Command, CommandMeta, Cx, Value, ValueKind};
use urfin_keymap::{ActiveKeymap, Catalog, Chord, KeyRow, Keymap, QsMode, WarnKind, key_risk};
use urfin_core::AppState;

use crate::UiState;
use crate::handler::{Part, arg, meta};

pub(crate) const PART: Part = Part {
    commands: COMMANDS,
    handle,
};

const COMMANDS: &[CommandMeta] = &[meta(
    "ui.keys",
    "Keyboard settings",
    &[
        // Курсор таблицы и клавиша в строке.
        arg("sel", ValueKind::Int, false),
        arg("chip", ValueKind::Int, false),
        arg("filter", ValueKind::Str, false),
        // Захват: `add` или `replace`; отмена захвата и вопроса.
        arg("capture", ValueKind::Str, false),
        arg("cancel", ValueKind::Bool, false),
        // Захваченное сочетание (приходит из `KeyInput`).
        arg("chord", ValueKind::Str, false),
        arg("confirm", ValueKind::Bool, false),
        arg("remove", ValueKind::Bool, false),
        arg("reset", ValueKind::Bool, false),
    ],
)];

/// Строка таблицы: переживает перечитывание раскладки и смену фильтра.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowId {
    key: String,
    place: &'static str,
}

impl RowId {
    fn of(r: &KeyRow) -> RowId {
        RowId {
            key: r.target.key(),
            place: r.place,
        }
    }

    pub fn is(&self, r: &KeyRow) -> bool {
        self.place == r.place && self.key == r.target.key()
    }
}

/// Идёт захват: какая строка и какая её клавиша заменяется.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    pub row: RowId,
    /// Контекст и сочетание заменяемой клавиши; `None` — новая.
    pub old: Option<(CompactString, CompactString)>,
}

/// Вопрос о конфликте: сочетание занято в пересекающемся контексте.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    pub capture: Capture,
    pub chord: CompactString,
    /// Команды, у которых клавиша будет снята.
    pub others: Vec<String>,
    pub risk: Option<&'static str>,
}

/// Пояснение под командой после назначения.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub row: RowId,
    pub key: &'static str,
}

/// Таблица для кадра: все строки и показанные поиском.
#[derive(Debug)]
pub struct KeysView {
    pub catalog: Arc<Catalog>,
    pub shown: Vec<u32>,
}

impl KeysView {
    /// Строка `i` показанных.
    pub fn row(&self, i: usize) -> Option<&KeyRow> {
        self.catalog.rows.get(*self.shown.get(i)? as usize)
    }
}

#[derive(Debug)]
struct Cached {
    generation: u64,
    lang: String,
    alt_letter: bool,
    filter: CompactString,
    view: Arc<KeysView>,
}

/// Состояние раздела в `AppState.ext`.
#[derive(Debug, Default)]
pub struct KeysUi {
    pub filter: CompactString,
    pub sel: usize,
    pub chip: usize,
    pub capture: Option<Capture>,
    pub ask: Option<Ask>,
    pub note: Option<Note>,
    cache: Mutex<Option<Cached>>,
}

fn ui<'c>(cx: &'c mut Cx<'_>) -> &'c mut KeysUi {
    if cx.state.ext.get::<KeysUi>().is_none() {
        cx.state.ext.insert(KeysUi::default());
    }
    cx.state.ext.get_mut::<KeysUi>().expect("inserted above")
}

/// Раздел открыт.
fn open(s: &AppState) -> bool {
    s.ext
        .get::<UiState>()
        .is_some_and(|u| u.settings.as_deref() == Some("keys"))
}

/// Идёт захват: нажатия — конвейеру в режиме захвата, диспетчер их не видит.
pub fn capturing(s: &AppState) -> bool {
    open(s) && s.ext.get::<KeysUi>().is_some_and(|k| k.capture.is_some())
}

/// Захват и вопрос закрыты: раздел сменился или закрыт.
pub(crate) fn stop(cx: &mut Cx<'_>) {
    let k = ui(cx);
    k.capture = None;
    k.ask = None;
    k.note = None;
}

/// Таблица (раскладка и язык те же — из кэша; новый фильтр считается по готовым строкам).
pub fn view(s: &AppState) -> Option<Arc<KeysView>> {
    let a = s.ext.get::<ActiveKeymap>()?;
    let i18n = urfin_i18n::current();
    let lang = i18n.lang().to_string();
    let alt_letter = urfin_keymap::qs_mode(&a.cfg) == QsMode::Alt;
    let k = s.ext.get::<KeysUi>();
    let filter = k.map(|k| k.filter.clone()).unwrap_or_default();
    let mut cache = k.map(|k| k.cache.lock().unwrap_or_else(PoisonError::into_inner));
    let old = cache.as_ref().and_then(|c| c.as_ref()).filter(|c| {
        c.generation == a.generation && c.lang == lang && c.alt_letter == alt_letter
    });
    if let Some(c) = old
        && c.filter == filter
    {
        return Some(Arc::clone(&c.view));
    }
    let catalog = old.map_or_else(
        || Arc::new(Catalog::build(&a.keymap, &i18n, alt_letter)),
        |c| Arc::clone(&c.view.catalog),
    );
    let shown = catalog.filter(&filter);
    let view = Arc::new(KeysView { catalog, shown });
    if let Some(c) = cache.as_mut() {
        **c = Some(Cached {
            generation: a.generation,
            lang,
            alt_letter,
            filter,
            view: Arc::clone(&view),
        });
    }
    Some(view)
}

fn handle(cmd: &Command, cx: &mut Cx<'_>) -> Option<Result<(), CmdError>> {
    if cmd.id.as_str() != "ui.keys" {
        return None;
    }
    run(cx, &cmd.args);
    Some(Ok(()))
}

fn int(a: &Args, name: &str) -> Option<usize> {
    a.int(name).map(|n| usize::try_from(n.max(0)).unwrap_or(0))
}

fn run(cx: &mut Cx<'_>, a: &Args) {
    let k = ui(cx);
    if let Some(f) = a.str("filter") {
        k.filter = f.into();
        k.sel = 0;
        k.chip = 0;
        k.note = None;
    }
    if let Some(n) = int(a, "sel") {
        if n != k.sel {
            k.chip = 0;
            k.note = None;
        }
        k.sel = n;
    }
    if let Some(n) = int(a, "chip") {
        k.chip = n;
    }
    if a.bool("cancel") == Some(true) {
        k.capture = None;
        k.ask = None;
    }
    if let Some(mode) = a.str("capture") {
        start(cx, mode == "replace");
    }
    if let Some(chord) = a.str("chord") {
        captured(cx, chord);
    }
    if a.bool("confirm") == Some(true) {
        confirm(cx);
    }
    if a.bool("remove") == Some(true) {
        remove(cx);
    }
    if a.bool("reset") == Some(true) {
        reset(cx);
    }
}

/// Строка под курсором.
fn current(s: &AppState) -> Option<(KeyRow, usize)> {
    let k = s.ext.get::<KeysUi>()?;
    let v = view(s)?;
    Some((v.row(k.sel)?.clone(), k.chip))
}

fn start(cx: &mut Cx<'_>, replace: bool) {
    let Some((row, chip)) = current(cx.state) else {
        return;
    };
    let old = replace
        .then(|| row.chips.get(chip).or(row.chips.first()))
        .flatten()
        .map(|c| (c.context.clone(), c.label.clone()));
    let k = ui(cx);
    k.ask = None;
    k.note = None;
    k.capture = Some(Capture {
        row: RowId::of(&row),
        old,
    });
}

/// Пояснение к сочетанию: что может пойти не так. Запись не блокируется (решение
/// 2026-11-05).
fn risk(km: &Keymap, chord: &Chord) -> Option<&'static str> {
    chord
        .0
        .iter()
        .find_map(|s| key_risk(*s, km.os()))
        .map(|w| match w {
            WarnKind::AltGrRisk => "ui-keys-risk-altgr",
            _ => "ui-keys-risk-reserved",
        })
}

fn row_of(s: &AppState, id: &RowId) -> Option<KeyRow> {
    let v = view(s)?;
    v.catalog.rows.iter().find(|r| id.is(r)).cloned()
}

/// Команда записи клавиши.
fn bind_cmd(row: &KeyRow, c: &Capture, chord: &str, steal: bool) -> Command {
    let list: Vec<Value> = row
        .target
        .cmds
        .iter()
        .map(|c| Value::Str(c.as_str().into()))
        .collect();
    let context = c.old.as_ref().map_or(&row.context, |(ctx, _)| ctx);
    let mut cmd = Command::new("keymap.bind")
        .arg("context", context.as_str())
        .arg("chord", chord)
        .arg("target", Value::List(list.into()));
    if let Some(r) = row.target.repeat {
        cmd = cmd.arg("repeat", r);
    }
    if let Some((ctx, old)) = &c.old {
        cmd = cmd.arg("old", old.as_str()).arg("old_context", ctx.as_str());
    }
    if steal {
        cmd = cmd.arg("steal", true);
    }
    cmd
}

/// Сочетание захвачено. Esc — отмена; занято — вопрос; иначе запись.
fn captured(cx: &mut Cx<'_>, text: &str) {
    let Some(cap) = ui(cx).capture.take() else {
        return;
    };
    let Some(km) = cx
        .state
        .ext
        .get::<ActiveKeymap>()
        .map(|a| Arc::clone(&a.keymap))
    else {
        return;
    };
    if text == "Esc" {
        return;
    }
    let (Ok(chord), Some(row)) = (Chord::parse(text, km.os()), row_of(cx.state, &cap.row)) else {
        return;
    };
    if row.chips.iter().any(|c| c.chord == chord) {
        return;
    }
    let context = cap.old.as_ref().map_or(&row.context, |(ctx, _)| ctx);
    let others = urfin_keymap::clashes(&km, &chord, context, &row.target);
    let label = chord.display(km.os());
    let note = risk(&km, &chord);
    if others.is_empty() {
        cx.dispatch(bind_cmd(&row, &cap, &label, false));
        ui(cx).note = note.map(|key| Note { row: cap.row, key });
        return;
    }
    let mut titles: Vec<String> = others.into_iter().map(|c| c.title).collect();
    titles.dedup();
    ui(cx).ask = Some(Ask {
        capture: cap,
        chord: label,
        others: titles,
        risk: note,
    });
}

fn confirm(cx: &mut Cx<'_>) {
    let Some(ask) = ui(cx).ask.take() else {
        return;
    };
    if let Some(row) = row_of(cx.state, &ask.capture.row) {
        cx.dispatch(bind_cmd(&row, &ask.capture, &ask.chord, true));
        ui(cx).note = ask.risk.map(|key| Note {
            row: ask.capture.row,
            key,
        });
    }
}

fn remove(cx: &mut Cx<'_>) {
    let Some((row, chip)) = current(cx.state) else {
        return;
    };
    if let Some(c) = row.chips.get(chip).or(row.chips.first()) {
        cx.dispatch(
            Command::new("keymap.unbind")
                .arg("context", c.context.as_str())
                .arg("chord", c.label.as_str()),
        );
        ui(cx).chip = 0;
    }
}

fn reset(cx: &mut Cx<'_>) {
    let Some((row, _)) = current(cx.state) else {
        return;
    };
    if row.changed {
        cx.dispatch(
            Command::new("keymap.reset")
                .arg("target", row.target.key())
                .arg("place", row.place),
        );
        ui(cx).chip = 0;
    }
}

/// Путь для статуса: начало — как в макете, `%APPDATA%\…`.
fn short(path: &std::path::Path) -> String {
    if let Some(app) = std::env::var_os("APPDATA")
        && let Ok(rest) = path.strip_prefix(&app)
    {
        return format!("%APPDATA%\\{}", rest.display());
    }
    path.display().to_string()
}

/// Статус раздела: файл keymap, конфликты, «Применяется сразу».
pub fn status(s: &AppState) -> Option<[String; 3]> {
    if !open(s) {
        return None;
    }
    let a = s.ext.get::<ActiveKeymap>()?;
    let i18n = urfin_i18n::current();
    let n = view(s).map_or(0, |v| v.catalog.conflicts);
    let mut args = urfin_i18n::FluentArgs::new();
    args.set("count", n);
    Some([
        short(&a.user_file),
        if n == 0 {
            String::new()
        } else {
            i18n.fmt("ui-keys-conflicts", &args)
        },
        i18n.get("ui-keys-applied"),
    ])
}

/// Клавиши окна: у захвата и вопроса — свои.
pub fn keys(s: &AppState) -> Option<Vec<(String, String)>> {
    if !open(s) {
        return None;
    }
    let i18n = urfin_i18n::current();
    let pair = |k: &str, v: &str| (k.to_owned(), i18n.get(v));
    let k = s.ext.get::<KeysUi>();
    if k.is_some_and(|k| k.capture.is_some()) {
        return Some(vec![pair("Esc", "ui-keys-cancel")]);
    }
    if k.is_some_and(|k| k.ask.is_some()) {
        return Some(vec![
            pair("Enter", "ui-keys-replace"),
            pair("Esc", "ui-keys-cancel"),
        ]);
    }
    Some(vec![
        pair("Tab", "ui-keys-section"),
        pair("Ctrl+F", "ui-keys-find-setting"),
        pair("Enter", "ui-keys-replace"),
        pair("Ins", "ui-keys-add"),
        pair("Del", "ui-keys-unbind"),
        pair("Backspace", "ui-keys-reset"),
        pair("Esc", "ui-keys-close"),
    ])
}

#[cfg(test)]
#[path = "keys_settings_tests.rs"]
pub(crate) mod tests;
