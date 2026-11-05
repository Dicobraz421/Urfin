//! Keymap: файлы TOML, слои и предупреждения (S3 §2.1–2.5, S3-03).
//!
//! Слои по возрастанию силы: `base < preset < os < user`. Файл — `format`, `name`,
//! `extends` (цепочка ≤ 4, цикл — предупреждение), `[context."<when>"]` и оверлей
//! платформы `[os.<windows|macos|linux>.context."<when>"]`. Значение привязки — строка
//! команды, таблица `{ cmd, …аргументы, _repeat }`, массив команд или `false` (снять
//! унаследованную; в `[context."*"]` — во всех контекстах).
//!
//! Ошибки не блокируют старт: битый файл пропускается, битая привязка — тоже, всё —
//! в [`KeymapWarning`]. Разрешение конфликтов при поиске — S3-04.

use std::path::Path;
use std::sync::Arc;

use compact_str::CompactString;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use urfin_core::{
    AppState, CmdError, Command, CommandMeta, CommandRegistry, ConfigWarning, Focus, Paths, Value,
};

use crate::compile::{
    ChordState, Compiled, EffectiveBinding, KeybarItem, Lookup, fkey_stroke, label_key,
};
use crate::key::{Chord, KeyCode, Mods, Os, Stroke};
use crate::when::{FlagRegistry, When, WhenErrorKind};

/// Встроенные пресеты.
const BUILTIN: [(&str, &str); 4] = [
    ("base", include_str!("../presets/base.toml")),
    ("far", include_str!("../presets/far.toml")),
    ("dn", include_str!("../presets/dn.toml")),
    ("urfin", include_str!("../presets/urfin.toml")),
];

/// Глубина `extends`.
const MAX_EXTENDS: usize = 4;

/// Секция `[keymap]` в `config.toml` (S3 §2.1, v4 №5).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeymapCfg {
    /// `far | dn | <имя файла из config_dir/keymaps/ без .toml>`.
    pub preset: CompactString,
    /// Пользовательский слой в `config_dir/keymaps/` (если файл есть).
    pub user: Option<String>,
    pub r#match: MatchMode,
    pub quick_search: QsMode,
    pub alt_tap_ms: u16,
    pub chord_timeout_ms: u16,
    pub mac_option_as_alt: OptionAlt,
}

impl Default for KeymapCfg {
    fn default() -> Self {
        KeymapCfg {
            preset: "far".into(),
            user: Some("user.toml".into()),
            r#match: MatchMode::Auto,
            quick_search: QsMode::Auto,
            alt_tap_ms: 400,
            chord_timeout_ms: 1500,
            mac_option_as_alt: OptionAlt::Left,
        }
    }
}

/// Сопоставление буквенных сочетаний (S0 v4 №5).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchMode {
    /// Латиница раскладки — по символу, иначе — по позиции US-QWERTY.
    Auto,
    Physical,
    Logical,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QsMode {
    Auto,
    Alt,
    AltTap,
    CtrlTap,
    Off,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionAlt {
    Left,
    Right,
    Both,
    None,
}

/// Слой привязки: более поздний сильнее при равной специфичности.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    Base,
    Preset,
    Os,
    User,
}

/// Откуда привязка: файл и секция.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Src {
    pub file: CompactString,
    pub context: CompactString,
}

/// Привязка: аккорд, контекст, команды.
#[derive(Clone, Debug)]
pub struct Binding {
    pub chord: Chord,
    pub when: Arc<When>,
    pub cmds: Arc<[Command]>,
    /// `_repeat` из привязки (`None` — не задан).
    pub repeat: Option<bool>,
    /// Автоповтор при удержании: `_repeat`, иначе `CommandMeta::repeatable` всех команд.
    pub repeatable: bool,
    pub layer: Layer,
    pub src: Src,
}

/// Предупреждение загрузки (S3 §2.5): где и что.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeymapWarning {
    pub file: CompactString,
    /// `[context] key` или пусто — файл целиком.
    pub at: CompactString,
    pub kind: WarnKind,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WarnKind {
    // Файлы.
    #[error("cannot read: {0}")]
    BadFile(CompactString),
    #[error("unsupported format {0} (expected 1)")]
    BadFormat(i64),
    #[error("unknown key")]
    UnknownKey,
    #[error("extends cycle: {0}")]
    ExtendsCycle(CompactString),
    #[error("extends chain longer than {MAX_EXTENDS}: {0}")]
    ExtendsTooDeep(CompactString),
    #[error("extends unknown keymap '{0}'")]
    ExtendsMissing(CompactString),
    #[error("keymap '{0}' is defined twice; the later one is ignored")]
    Duplicate(CompactString),
    // Ввод.
    #[error("unknown command '{0}'")]
    UnknownCommand(CompactString),
    #[error("bad arguments: {0}")]
    BadArgs(CompactString),
    #[error("bad key: {0}")]
    BadKey(CompactString),
    #[error("bad context: {0}")]
    BadWhen(CompactString),
    #[error("unknown flag '{0}'")]
    UnknownFlag(CompactString),
    // Коллизии.
    #[error("same key and specificity as [{0}] in this file; the later one wins")]
    Ambiguous(CompactString),
    #[error("chord '{0}' starts with this key; this single binding is disabled")]
    PrefixShadow(CompactString),
    // Запрещённые.
    #[error("reserved by the OS: never reaches Urfin")]
    Reserved,
    #[error("Ctrl+Alt+printable is AltGr in many layouts")]
    AltGrRisk,
}

impl std::fmt::Display for KeymapWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.at.is_empty() {
            write!(f, "{}: {}", self.file, self.kind)
        } else {
            write!(f, "{}: {}: {}", self.file, self.at, self.kind)
        }
    }
}

impl KeymapWarning {
    /// Встроенный пресет ссылается на команду, которой ещё нет: её крейт (сессия)
    /// не собран или не зарегистрирован. Не ошибка пользователя — не показывается.
    pub fn missing_provider(&self) -> bool {
        matches!(self.kind, WarnKind::UnknownCommand(_))
            && BUILTIN.iter().any(|(n, _)| *n == self.file)
    }

    /// Только пояснение: сочетание рискованное, но выбрано осознанно. Баннера нет —
    /// пояснение уже показано при назначении (решение 2026-11-05).
    pub fn advisory(&self) -> bool {
        matches!(self.kind, WarnKind::Reserved | WarnKind::AltGrRisk)
            && !BUILTIN.iter().any(|(n, _)| *n == self.file)
    }

    /// Для баннера конфига (S1 `ConfigWarning`).
    pub fn to_config(&self) -> ConfigWarning {
        ConfigWarning {
            key: format!("keymap.{}", self.file),
            msg: if self.at.is_empty() {
                self.kind.to_string()
            } else {
                format!("{}: {}", self.at, self.kind)
            },
        }
    }
}

/// Скомпилированный keymap: привязки после слоёв, `extends` и снятий. Поиск — S3-04.
#[derive(Debug)]
pub struct Keymap {
    os: Os,
    bindings: Vec<Binding>,
    warnings: Vec<KeymapWarning>,
    compiled: Compiled,
    /// Зарегистрированные команды и принимают ли они `text` (вставка, печать).
    commands: rustc_hash::FxHashMap<&'static str, bool>,
    /// Реестр команд в порядке регистрации (раздел «Клавиатура»).
    metas: Vec<&'static CommandMeta>,
    /// Привязки до пользовательского файла: то, к чему возвращает сброс.
    preset: Vec<Binding>,
}

impl Keymap {
    /// Пресет `cfg.preset` (вшитый или `config_dir/keymaps/<имя>.toml`) с цепочкой
    /// `extends` и пользовательский слой `config_dir/keymaps/<cfg.user>`. Не падает.
    pub fn load(
        paths: &Paths,
        cfg: &KeymapCfg,
        reg: &CommandRegistry,
        flags: &FlagRegistry,
        os: Os,
    ) -> (Arc<Keymap>, Vec<KeymapWarning>) {
        let dir = paths.config_dir.join("keymaps");
        let mut b = Builder::new(reg, flags, os);
        let source = |name: &str, b: &mut Builder<'_>| -> Option<(String, Layer)> {
            let file = dir.join(format!("{name}.toml"));
            let user = read(&file, name, b);
            let builtin = BUILTIN.iter().find(|(n, _)| *n == name);
            match (user, builtin) {
                (Some(text), Some(_)) => {
                    b.warn(name, "", WarnKind::Duplicate(name.into()));
                    Some((text, Layer::User))
                }
                (Some(text), None) => Some((text, Layer::User)),
                (None, Some((n, text))) => Some(((*text).to_owned(), builtin_layer(n))),
                (None, None) => None,
            }
        };
        let user = cfg.user.as_deref().and_then(|u| {
            let name = u.strip_suffix(".toml").unwrap_or(u);
            read(&dir.join(format!("{name}.toml")), name, &mut b).map(|t| (name.to_owned(), t))
        });
        b.chain(&cfg.preset, &source);
        if let Some((name, text)) = user {
            b.user_file(&name, &text, &source);
        }
        b.finish()
    }

    /// Слои из строк — для тестов: `extends` ищется среди этих слоёв по `name` и среди
    /// вшитых пресетов.
    pub fn from_str_layers(
        layers: &[(&str, Layer)],
        reg: &CommandRegistry,
        flags: &FlagRegistry,
        os: Os,
    ) -> (Arc<Keymap>, Vec<KeymapWarning>) {
        let mut b = Builder::new(reg, flags, os);
        let mut seen: Vec<CompactString> = Vec::new();
        let mut named: Vec<(CompactString, String, Layer)> = Vec::new();
        for (i, (text, layer)) in layers.iter().enumerate() {
            let name = file_name(text).unwrap_or_else(|| format!("layer{i}").into());
            if *layer == Layer::User && b.user.is_none() {
                b.user = Some(name.clone());
            }
            if seen.contains(&name) {
                b.warn(&name, "", WarnKind::Duplicate(name.clone()));
                continue;
            }
            seen.push(name.clone());
            named.push((name, (*text).to_owned(), *layer));
        }
        let source = |name: &str, _: &mut Builder<'_>| -> Option<(String, Layer)> {
            named
                .iter()
                .find(|(n, _, _)| n == name)
                .map(|(_, t, l)| (t.clone(), *l))
                .or_else(|| {
                    BUILTIN
                        .iter()
                        .find(|(n, _)| *n == name)
                        .map(|(n, t)| ((*t).to_owned(), builtin_layer(n)))
                })
        };
        // Файлы, которые расширяют другие из списка, грузятся через свою цепочку.
        for (name, text, layer) in &named {
            if b.loaded.contains(name) {
                continue;
            }
            b.file_with_chain(name, text, *layer, &source);
        }
        b.finish()
    }

    pub fn os(&self) -> Os {
        self.os
    }

    /// Привязки в порядке загрузки (поздние сильнее).
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    pub fn warnings(&self) -> &[KeymapWarning] {
        &self.warnings
    }

    /// Нажатие `s` (после начатого аккорда `pending`) в состоянии `st`. 0 аллокаций.
    pub fn lookup(&self, pending: Option<&ChordState>, s: Stroke, st: &AppState) -> Lookup<'_> {
        self.compiled.lookup(pending, s, st, st.focus)
    }

    /// Поиск так, будто фокус — `focus` (флаги — по `st`).
    pub fn lookup_as(
        &self,
        pending: Option<&ChordState>,
        s: Stroke,
        st: &AppState,
        focus: Focus,
    ) -> Lookup<'_> {
        self.compiled.lookup(pending, s, st, focus)
    }

    /// Сочетания, которые сейчас вызывают ровно `cmd` (меню и палитра S2).
    pub fn keys_for(&self, cmd: &Command, st: &AppState) -> SmallVec<[Chord; 2]> {
        self.bindings
            .iter()
            .enumerate()
            .filter(|(_, b)| b.cmds.len() == 1 && b.cmds[0] == *cmd)
            .filter(|(i, b)| {
                self.compiled
                    .wins(&b.chord, u32::try_from(*i).unwrap_or(u32::MAX), st)
            })
            .map(|(_, b)| b.chord.clone())
            .collect()
    }

    /// F1…F12 для модификаторов `mods` в состоянии `st` (keybar S2): одиночная команда.
    pub fn keybar(&self, mods: Mods, st: &AppState) -> [Option<KeybarItem>; 12] {
        std::array::from_fn(|i| match self.lookup(None, fkey_stroke(i, mods), st) {
            Lookup::Hit([cmd], _) => Some(KeybarItem {
                label_key: label_key(cmd),
                cmd: cmd.clone(),
            }),
            _ => None,
        })
    }

    /// Команда зарегистрирована (конвейер не шлёт команд сессий, которых ещё нет).
    pub fn has_command(&self, id: &str) -> bool {
        self.commands.contains_key(id)
    }

    /// Команда принимает аргумент `text` (`cmdline.paste` и т. п.).
    pub fn takes_text(&self, id: &str) -> bool {
        self.commands.get(id).copied().unwrap_or(false)
    }

    /// Реестр команд в порядке регистрации.
    pub fn metas(&self) -> &[&'static CommandMeta] {
        &self.metas
    }

    /// Привязки без пользовательского файла.
    pub fn preset_bindings(&self) -> &[Binding] {
        &self.preset
    }

    /// Все привязки после слоёв и снятий (`keymap.show`, `keymap.dump`).
    pub fn effective(&self) -> impl Iterator<Item = EffectiveBinding<'_>> {
        self.bindings.iter().map(|b| EffectiveBinding {
            chord: &b.chord,
            binding: b,
        })
    }
}

/// Файл пользовательского слоя: `config_dir/keymaps/<cfg.user>.toml`, по умолчанию
/// `user.toml` (если слой выключен, первая правка из настроек его включает).
pub fn user_path(paths: &Paths, cfg: &KeymapCfg) -> std::path::PathBuf {
    let name = cfg.user.as_deref().unwrap_or("user.toml");
    let name = name.strip_suffix(".toml").unwrap_or(name);
    paths.config_dir.join("keymaps").join(format!("{name}.toml"))
}

fn builtin_layer(name: &str) -> Layer {
    if name == "base" {
        Layer::Base
    } else {
        Layer::Preset
    }
}

fn read(path: &Path, name: &str, b: &mut Builder<'_>) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            b.warn(name, "", WarnKind::BadFile(e.to_string().into()));
            None
        }
    }
}

/// `name` файла без полного разбора (для поиска `extends`).
fn file_name(text: &str) -> Option<CompactString> {
    let t: toml::Table = toml::from_str(text).ok()?;
    t.get("name")?.as_str().map(Into::into)
}

/// Привязка или снятие из файла.
enum Entry {
    Bind(Binding),
    Unbind { chord: Chord, when: Arc<When> },
}

struct Builder<'a> {
    reg: &'a CommandRegistry,
    flags: &'a FlagRegistry,
    os: Os,
    bindings: Vec<Binding>,
    warnings: Vec<KeymapWarning>,
    loaded: Vec<CompactString>,
    /// Имя пользовательского файла: перед ним снимается `preset`.
    user: Option<CompactString>,
    preset: Option<Vec<Binding>>,
}

type Source<'s> = dyn Fn(&str, &mut Builder<'_>) -> Option<(String, Layer)> + 's;

impl<'a> Builder<'a> {
    fn new(reg: &'a CommandRegistry, flags: &'a FlagRegistry, os: Os) -> Self {
        Builder {
            reg,
            flags,
            os,
            bindings: Vec::new(),
            warnings: Vec::new(),
            loaded: Vec::new(),
            user: None,
            preset: None,
        }
    }

    fn warn(&mut self, file: &str, at: &str, kind: WarnKind) {
        self.warnings.push(KeymapWarning {
            file: file.into(),
            at: at.into(),
            kind,
        });
    }

    /// Пресет `name` с его цепочкой `extends`.
    fn chain(&mut self, name: &str, source: &Source<'_>) {
        let Some((text, layer)) = source(name, self) else {
            self.warn(name, "", WarnKind::ExtendsMissing(name.into()));
            return;
        };
        self.file_with_chain(name, &text, layer, source);
    }

    /// Пользовательский файл: со своим `extends` — через цепочку, иначе поверх пресета.
    fn user_file(&mut self, name: &str, text: &str, source: &Source<'_>) {
        self.user = Some(name.into());
        self.file_with_chain(name, text, Layer::User, source);
    }

    /// Сначала предки по `extends` (от корня), затем сам файл.
    fn file_with_chain(&mut self, name: &str, text: &str, layer: Layer, source: &Source<'_>) {
        let mut chain: Vec<(CompactString, String, Layer)> =
            vec![(name.into(), text.to_owned(), layer)];
        let mut names: Vec<CompactString> = vec![name.into()];
        loop {
            let Some(parent) = extends_of(&chain.last().expect("non-empty").1) else {
                break;
            };
            if self.loaded.contains(&parent) {
                break;
            }
            if names.contains(&parent) {
                names.push(parent);
                let path = names
                    .iter()
                    .map(CompactString::as_str)
                    .collect::<Vec<_>>()
                    .join(" → ");
                self.warn(name, "", WarnKind::ExtendsCycle(path.into()));
                break;
            }
            if chain.len() > MAX_EXTENDS {
                names.push(parent);
                let path = names
                    .iter()
                    .map(CompactString::as_str)
                    .collect::<Vec<_>>()
                    .join(" → ");
                self.warn(name, "", WarnKind::ExtendsTooDeep(path.into()));
                break;
            }
            let Some((ptext, player)) = source(&parent, self) else {
                let last = chain.last().expect("non-empty").0.clone();
                self.warn(&last, "", WarnKind::ExtendsMissing(parent));
                break;
            };
            names.push(parent.clone());
            chain.push((parent, ptext, player));
        }
        for (n, t, l) in chain.into_iter().rev() {
            if self.loaded.contains(&n) {
                continue;
            }
            if self.user.as_ref() == Some(&n) && self.preset.is_none() {
                self.preset = Some(self.bindings.clone());
            }
            self.loaded.push(n.clone());
            let entries = self.parse_file(&n, &t, l);
            self.apply(entries);
        }
    }

    fn apply(&mut self, entries: Vec<Entry>) {
        for e in entries {
            match e {
                Entry::Bind(b) => self.bindings.push(b),
                Entry::Unbind { chord, when } => {
                    let key = norm(when.source());
                    self.bindings.retain(|b| {
                        b.chord != chord || !(when.is_any() || norm(b.when.source()) == key)
                    });
                }
            }
        }
    }

    /// Привязки файла; битый файл — предупреждение и пустой список.
    fn parse_file(&mut self, file: &str, text: &str, layer: Layer) -> Vec<Entry> {
        let table: toml::Table = match toml::from_str(text) {
            Ok(t) => t,
            Err(e) => {
                let msg: String = e.message().into();
                self.warn(file, "", WarnKind::BadFile(msg.into()));
                return Vec::new();
            }
        };
        if let Some(v) = table.get("format") {
            match v.as_integer() {
                Some(1) => {}
                n => {
                    self.warn(file, "", WarnKind::BadFormat(n.unwrap_or(-1)));
                    return Vec::new();
                }
            }
        }
        let mut entries = Vec::new();
        for (key, value) in &table {
            match key.as_str() {
                "format" | "name" | "extends" => {}
                "context" => self.contexts(file, value, layer, &mut entries),
                "os" => {
                    let Some(oses) = value.as_table() else {
                        self.warn(file, "os", WarnKind::UnknownKey);
                        continue;
                    };
                    for (os, body) in oses {
                        let this = match os.as_str() {
                            "windows" => Os::Windows,
                            "macos" => Os::Macos,
                            "linux" => Os::Linux,
                            _ => {
                                self.warn(file, &format!("os.{os}"), WarnKind::UnknownKey);
                                continue;
                            }
                        };
                        if this != self.os {
                            continue;
                        }
                        let os_layer = if layer == Layer::User {
                            Layer::User
                        } else {
                            Layer::Os
                        };
                        match body.as_table().and_then(|t| t.get("context")) {
                            Some(c) => self.contexts(file, c, os_layer, &mut entries),
                            None => self.warn(file, &format!("os.{os}"), WarnKind::UnknownKey),
                        }
                    }
                }
                other => self.warn(file, other, WarnKind::UnknownKey),
            }
        }
        self.ambiguity(file, &entries);
        entries
    }

    fn contexts(&mut self, file: &str, value: &toml::Value, layer: Layer, out: &mut Vec<Entry>) {
        let Some(contexts) = value.as_table() else {
            self.warn(file, "context", WarnKind::UnknownKey);
            return;
        };
        for (ctx, body) in contexts {
            let at = format!("[{ctx}]");
            let when = match When::parse(ctx, self.flags) {
                Ok(w) => Arc::new(w),
                Err(e) => {
                    let kind = match e.kind {
                        WhenErrorKind::UnknownFlag(f) => WarnKind::UnknownFlag(f),
                        _ => WarnKind::BadWhen(e.to_string().into()),
                    };
                    self.warn(file, &at, kind);
                    continue;
                }
            };
            let Some(binds) = body.as_table() else {
                self.warn(file, &at, WarnKind::UnknownKey);
                continue;
            };
            for (keys, v) in binds {
                let at = format!("[{ctx}] {keys}");
                let chord = match Chord::parse(keys, self.os) {
                    Ok(c) => c,
                    Err(e) => {
                        self.warn(file, &at, WarnKind::BadKey(e.to_string().into()));
                        continue;
                    }
                };
                if *v == toml::Value::Boolean(false) {
                    out.push(Entry::Unbind {
                        chord,
                        when: Arc::clone(&when),
                    });
                    continue;
                }
                if when.is_any() {
                    self.warn(
                        file,
                        &at,
                        WarnKind::BadWhen("'*' only unbinds (= false)".into()),
                    );
                    continue;
                }
                let Some((cmds, repeat)) = self.commands(file, &at, v) else {
                    continue;
                };
                let repeatable = repeat.unwrap_or_else(|| {
                    cmds.iter()
                        .all(|c| self.reg.get(c.id.as_str()).is_some_and(|m| m.repeatable))
                });
                // Встроенные слои проверяются в `finish`: их os-оверлей снимает
                // системные сочетания своей ОС.
                if layer == Layer::User {
                    for s in &chord.0 {
                        if let Some(kind) = key_risk(*s, self.os) {
                            self.warn(file, &at, kind);
                        }
                    }
                }
                out.push(Entry::Bind(Binding {
                    chord,
                    when: Arc::clone(&when),
                    cmds,
                    repeat,
                    repeatable,
                    layer,
                    src: Src {
                        file: file.into(),
                        context: ctx.as_str().into(),
                    },
                }));
            }
        }
    }

    /// Значение привязки: строка, таблица или массив; проверка по реестру.
    fn commands(
        &mut self,
        file: &str,
        at: &str,
        v: &toml::Value,
    ) -> Option<(Arc<[Command]>, Option<bool>)> {
        let mut repeat = None;
        let mut out: SmallVec<[Command; 2]> = SmallVec::new();
        let items: SmallVec<[&toml::Value; 2]> = match v {
            toml::Value::Array(a) if !a.is_empty() => a.iter().collect(),
            toml::Value::Array(_) => {
                self.warn(file, at, WarnKind::BadArgs("empty command list".into()));
                return None;
            }
            other => std::iter::once(other).collect(),
        };
        for item in items {
            let cmd = match item {
                toml::Value::String(s) => Command::parse(s).map_err(|e| e.to_string()),
                toml::Value::Table(t) => table_command(t, &mut repeat),
                _ => Err("expected a command string, a { cmd = … } table or false".to_owned()),
            };
            let cmd = match cmd {
                Ok(c) => c,
                Err(e) => {
                    self.warn(file, at, WarnKind::BadArgs(e.into()));
                    return None;
                }
            };
            if let Err(e) = self.reg.validate(&cmd) {
                let kind = match e {
                    CmdError::Unknown(id) => WarnKind::UnknownCommand(id.as_str().into()),
                    other => WarnKind::BadArgs(other.to_string().into()),
                };
                self.warn(file, at, kind);
                return None;
            }
            out.push(cmd);
        }
        Some((out.into_iter().collect(), repeat))
    }

    /// `Ambiguous`: в одном файле одна клавиша в контекстах, которые могут сработать
    /// вместе с равной специфичностью.
    fn ambiguity(&mut self, file: &str, entries: &[Entry]) {
        let binds: Vec<&Binding> = entries
            .iter()
            .filter_map(|e| match e {
                Entry::Bind(b) => Some(b),
                Entry::Unbind { .. } => None,
            })
            .collect();
        let mut found = Vec::new();
        for (i, a) in binds.iter().enumerate() {
            for b in &binds[i + 1..] {
                if a.chord == b.chord
                    && a.src.context != b.src.context
                    && a.when.ambiguous_with(&b.when)
                {
                    found.push((
                        format!("[{}] {}", b.src.context, b.chord),
                        a.src.context.clone(),
                    ));
                }
            }
        }
        for (at, other) in found {
            self.warn(file, &at, WarnKind::Ambiguous(other));
        }
    }

    /// `PrefixShadow`: одиночная привязка, совпадающая с началом аккорда в пересекающемся
    /// контексте, отключается.
    fn finish(mut self) -> (Arc<Keymap>, Vec<KeymapWarning>) {
        let mut late = Vec::new();
        for b in self.bindings.iter().filter(|b| b.layer != Layer::User) {
            for s in &b.chord.0 {
                if let Some(kind) = key_risk(*s, self.os) {
                    let at = format!("[{}] {}", b.src.context, b.chord);
                    late.push(KeymapWarning {
                        file: b.src.file.clone(),
                        at: at.into(),
                        kind,
                    });
                }
            }
        }
        self.warnings.extend(late);
        let mut shadowed = Vec::new();
        for (i, single) in self.bindings.iter().enumerate() {
            if single.chord.0.len() != 1 {
                continue;
            }
            let s = single.chord.0[0];
            if let Some(chord) = self.bindings.iter().find(|c| {
                c.chord.0.len() > 1
                    && c.chord.0[0] == s
                    && (norm(c.when.source()) == norm(single.when.source())
                        || c.when.ambiguous_with(&single.when))
            }) {
                shadowed.push((i, chord.chord.clone()));
            }
        }
        for (i, chord) in shadowed.iter().rev() {
            let b = self.bindings.remove(*i);
            let at = format!("[{}] {}", b.src.context, b.chord);
            self.warnings.push(KeymapWarning {
                file: b.src.file.clone(),
                at: at.into(),
                kind: WarnKind::PrefixShadow(chord.to_string().into()),
            });
        }
        let warnings = self.warnings.clone();
        for w in &warnings {
            if w.missing_provider() {
                tracing::debug!(target: "keymap", "{w}");
            } else {
                tracing::warn!(target: "keymap", "{w}");
            }
        }
        (
            Arc::new(Keymap {
                os: self.os,
                metas: self.reg.iter().collect(),
                preset: self.preset.unwrap_or_else(|| self.bindings.clone()),
                commands: self
                    .reg
                    .iter()
                    .map(|m| (m.id, m.args.iter().any(|a| a.name == "text")))
                    .collect(),
                compiled: Compiled::build(&self.bindings),
                bindings: self.bindings,
                warnings: self.warnings,
            }),
            warnings,
        )
    }
}

fn extends_of(text: &str) -> Option<CompactString> {
    let t: toml::Table = toml::from_str(text).ok()?;
    t.get("extends")?.as_str().map(Into::into)
}

/// Контекст для сравнения: без пробелов.
pub(crate) fn norm(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Таблица `{ cmd = "id", арг = значение, _repeat = bool }`.
fn table_command(t: &toml::Table, repeat: &mut Option<bool>) -> Result<Command, String> {
    let id = t
        .get("cmd")
        .and_then(toml::Value::as_str)
        .ok_or("table without cmd = \"…\"")?;
    let mut cmd = Command::parse(id).map_err(|e| e.to_string())?;
    for (k, v) in t {
        match k.as_str() {
            "cmd" => {}
            "_repeat" => *repeat = Some(v.as_bool().ok_or("_repeat must be true or false")?),
            _ => cmd = cmd.arg(k, value(v)?),
        }
    }
    Ok(cmd)
}

fn value(v: &toml::Value) -> Result<Value, String> {
    Ok(match v {
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Integer(i) => Value::Int(*i),
        toml::Value::String(s) => Value::Str(s.as_str().into()),
        toml::Value::Array(a) => {
            Value::List(a.iter().map(value).collect::<Result<Vec<_>, _>>()?.into())
        }
        other => return Err(format!("unsupported value {other}")),
    })
}

/// Исключение из `AltGrRisk`, заданное владельцем: Ctrl+Alt+H — подсветка вывода
/// терминала, Ctrl+Alt+R — запись сеанса, Ctrl+Alt+G — состав группы мультивыполнения
/// (S0-decisions №88, 2026-10-23). Ровно Ctrl+Alt, без Shift.
pub fn altgr_owner_ok(s: Stroke) -> bool {
    let m = s.mods;
    matches!(
        s.key,
        KeyCode::Key(crate::ev::Key::H | crate::ev::Key::R | crate::ev::Key::G)
    ) && !m.intersects(Mods::SHIFT | Mods::CMD)
        && m.intersects(Mods::CTRL | Mods::LCTRL | Mods::RCTRL)
        && m.intersects(Mods::ALT | Mods::LALT | Mods::RALT)
}

/// Рискованные сочетания: `Reserved` (S3 §4), `AltGrRisk` (Windows, Ctrl+Alt+печатная;
/// кроме [`altgr_owner_ok`]).
///
/// Из интерфейса такое сочетание назначается свободно (решение 2026-11-05): это повод
/// для пояснения, а не запрет. Загрузка привязку не отбрасывает; для встроенных
/// пресетов предупреждение — проверка разработчика.
pub fn key_risk(s: Stroke, os: Os) -> Option<WarnKind> {
    use crate::ev::Key as K;
    let m = s.mods;
    let ctrl = m.intersects(Mods::CTRL | Mods::LCTRL | Mods::RCTRL);
    let alt = m.intersects(Mods::ALT | Mods::LALT | Mods::RALT);
    let shift = m.contains(Mods::SHIFT);
    let cmd = m.contains(Mods::CMD);
    let key = match s.key {
        KeyCode::Key(k) => Some(k),
        _ => None,
    };
    let is = |k: K| key == Some(k);
    let f = |n: u8| key.is_some_and(|k| fkey(k) == Some(n));
    let reserved = match os {
        Os::Windows => {
            // Alt+Shift+F4 — альтернативный редактор (S3 §4): закрывает только Alt+F4.
            cmd || (alt
                && !ctrl
                && (is(K::Tab)
                    || is(K::Escape)
                    || (is(K::F4) && !shift)
                    || is(K::Space)
                    || is(K::Backtick)))
                || (ctrl && !alt && !shift && is(K::Escape))
                || (ctrl && shift && !alt && is(K::Escape))
                || (ctrl && alt && is(K::Delete))
                || (!alt && !cmd && (ctrl ^ shift) && is(K::Space))
                || is(K::PrintScreen)
        }
        Os::Macos => {
            (cmd && (is(K::Tab) || is(K::Backtick) || is(K::Space)))
                || (ctrl && !cmd && is(K::Space))
                || (ctrl
                    && !cmd
                    && !alt
                    && !shift
                    && (is(K::ArrowUp)
                        || is(K::ArrowDown)
                        || is(K::ArrowLeft)
                        || is(K::ArrowRight)))
                || (ctrl && !cmd && !alt && !shift && (1..=8).any(f))
                || (cmd && shift && (is(K::Num3) || is(K::Num4) || is(K::Num5)))
                || (ctrl && cmd && is(K::Q))
                || (cmd && alt && (is(K::Escape) || is(K::D)))
        }
        Os::Linux => {
            cmd || (alt && !ctrl && !shift && [1, 2, 7, 8, 10].into_iter().any(f))
                || (ctrl && alt && ((1..=12).any(f) || is(K::T)))
        }
    };
    if reserved {
        return Some(WarnKind::Reserved);
    }
    if os == Os::Windows && ctrl && alt && !cmd && key.is_some_and(printable) && !altgr_owner_ok(s)
    {
        return Some(WarnKind::AltGrRisk);
    }
    None
}

fn fkey(k: crate::ev::Key) -> Option<u8> {
    use crate::ev::Key as K;
    const F: [K; 12] = [
        K::F1,
        K::F2,
        K::F3,
        K::F4,
        K::F5,
        K::F6,
        K::F7,
        K::F8,
        K::F9,
        K::F10,
        K::F11,
        K::F12,
    ];
    F.iter()
        .position(|f| *f == k)
        .and_then(|i| u8::try_from(i + 1).ok())
}

/// Печатная клавиша: буквы, цифры, знаки основного блока.
fn printable(k: crate::ev::Key) -> bool {
    let name = k.name();
    let single = name.chars().count() == 1;
    single
        || matches!(
            k,
            crate::ev::Key::Minus
                | crate::ev::Key::Plus
                | crate::ev::Key::Equals
                | crate::ev::Key::OpenBracket
                | crate::ev::Key::CloseBracket
                | crate::ev::Key::Backslash
                | crate::ev::Key::Semicolon
                | crate::ev::Key::Quote
                | crate::ev::Key::Comma
                | crate::ev::Key::Period
                | crate::ev::Key::Slash
                | crate::ev::Key::Backtick
        )
}

#[cfg(test)]
#[path = "keymap_tests.rs"]
mod tests;
