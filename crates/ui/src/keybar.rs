//! Строка F-клавиш (S2 §2.12, S2-14) и подсказки клавиш [`KeyHints`].
//!
//! Привязки — заглушка пресета Far из S3 §3 ([`FarStub`]), пока keymap S3 (S3-04,
//! `Keymap::keybar`) не реализует `KeyHints`. Зажатые Shift / Ctrl / Alt меняют
//! подписи в том же кадре. Подпись — ключ слота (`keybar-<id>[-<вариант>]`), запасная —
//! `cmd-<id>` (S0 v3 №14). Слот с незарегистрированной командой — серый, без клика.

use compact_str::CompactString;
use urfin_core::{AppState, Command, CommandRegistry};
use urfin_i18n::I18n;

/// Слотов в строке (S2 §2.12: F1…F10).
pub const SLOTS: u8 = 10;

/// Набор модификаторов для строки F-клавиш.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Mods {
    #[default]
    None,
    Shift,
    Ctrl,
    Alt,
}

impl Mods {
    fn idx(self) -> usize {
        self as usize
    }

    fn prefix(self) -> &'static str {
        match self {
            Mods::None => "",
            Mods::Shift => "Shift+",
            Mods::Ctrl => "Ctrl+",
            Mods::Alt => "Alt+",
        }
    }
}

/// F-клавиша: команда (с аргументами) и ключ подписи.
#[derive(Clone, Debug, PartialEq)]
pub struct FKey {
    pub cmd: Command,
    pub label_key: CompactString,
}

/// Подсказки клавиш для keybar и меню (реализует keymap S3, до него — [`FarStub`]).
///
/// Отличие от S2 §9: слот — [`FKey`] с командой и аргументами, а не только id: в Far
/// Ctrl+F3…F7 — один `panel.sort` с разными `key`, и подписи у них разные.
pub trait KeyHints: std::fmt::Debug {
    /// F1…F12 для набора модификаторов.
    fn fkeys(&self, mods: Mods) -> [Option<FKey>; 12];
    /// Сочетание для команды (`F5`, `Shift+F6`, `Ctrl+1`), если есть.
    fn hint(&self, cmd: &Command) -> Option<CompactString>;
}

/// Пресет Far (S3 §3): команда с аргументами и ключ подписи по F-клавишам.
const FAR: [[Option<(&str, &str)>; 12]; 4] = [
    [
        Some(("ui.help", "keybar-ui-help")),
        Some(("ui.user_menu", "keybar-ui-user_menu")),
        Some(("viewer.open", "keybar-viewer-open")),
        Some(("editor.open", "keybar-editor-open")),
        Some(("ops.copy", "keybar-ops-copy")),
        Some(("ops.move", "keybar-ops-move")),
        Some(("ops.mkdir", "keybar-ops-mkdir")),
        Some(("ops.delete", "keybar-ops-delete")),
        Some(("ui.menu", "keybar-ui-menu")),
        Some(("app.quit", "keybar-app-quit")),
        Some(("ui.palette", "cmd-ui-palette")),
        Some(("ui.screens", "cmd-ui-screens")),
    ],
    [
        Some(("arc.add", "keybar-arc-add")),
        Some(("arc.extract", "keybar-arc-extract")),
        Some(("arc.commands", "keybar-arc-commands")),
        None,
        Some(("ops.copy current=true", "keybar-ops-copy-current")),
        Some(("ops.rename", "keybar-ops-rename")),
        None,
        Some(("ops.delete current=true", "keybar-ops-delete-current")),
        Some(("app.save_session", "keybar-app-save_session")),
        None,
        None,
        None,
    ],
    [
        Some(("panel.toggle side=left", "keybar-panel-toggle-left")),
        Some(("panel.toggle side=right", "keybar-panel-toggle-right")),
        Some(("panel.sort key=name", "keybar-panel-sort-name")),
        Some(("panel.sort key=ext", "keybar-panel-sort-ext")),
        Some(("panel.sort key=mtime", "keybar-panel-sort-mtime")),
        Some(("panel.sort key=size", "keybar-panel-sort-size")),
        Some(("panel.sort key=unsorted", "keybar-panel-sort-unsorted")),
        None,
        None,
        None,
        None,
        Some(("ui.menu.sort", "cmd-ui-menu-sort")),
    ],
    [
        Some(("ui.drives side=left", "keybar-ui-drives-left")),
        Some(("ui.drives side=right", "keybar-ui-drives-right")),
        None,
        None,
        None,
        Some(("ops.link", "keybar-ops-link")),
        Some(("search.files", "keybar-search-files")),
        None,
        Some(("ui.window.maximize", "keybar-ui-window-maximize")),
        Some(("search.dirs", "keybar-search-dirs")),
        None,
        Some(("ui.history.dirs", "keybar-ui-history-dirs")),
    ],
];

/// Сочетания без F-клавиш, которые показывает меню (S3 §3, пресет Far; временные
/// клавиши app до keymap S3).
const FAR_EXTRA: &[(&str, &str)] = &[
    ("panel.view_mode mode=brief", "Ctrl+1"),
    ("panel.view_mode mode=medium", "Ctrl+2"),
    ("panel.view_mode mode=full", "Ctrl+3"),
    ("panel.view_mode mode=wide", "Ctrl+4"),
    ("ui.panels.toggle_all", "Ctrl+O"),
    ("panel.switch", "Tab"),
    ("sel.toggle", "Ins"),
    ("sel.invert", "Gray *"),
    ("ui.draft command=sel.add arg=mask", "Gray +"),
    ("ui.draft command=sel.remove arg=mask", "Gray −"),
    ("tab.new", "Ctrl+Shift+T"),
    ("tab.close", "Ctrl+Shift+W"),
    ("panel.swap", "Ctrl+U"),
];

/// Клавиши экрана поиска (`screen-search`, S8 §2.5): команда, клавиша по умолчанию и подпись.
/// Enter различает сам экран (форма или список), его нет в keymap.
const SEARCH_KEYS: [(&str, &str, &str); 6] = [
    ("", "Enter", "keybar-search-goto"),
    ("search.view", "F3", "keybar-search-view"),
    ("search.edit", "F4", "keybar-search-edit"),
    ("search.to_panel", "Ctrl+Enter", "keybar-search-to_panel"),
    ("search.files", "Alt+F7", "keybar-search-files-new"),
    ("search.close", "Esc", "keybar-search-close"),
];

/// Клавиши экрана поиска: своя клавиша — из активной раскладки. В форме Enter запускает
/// поиск, в списке — переходит к файлу.
pub fn search_keys(s: &AppState, form: bool) -> Vec<(String, String)> {
    let i18n = urfin_i18n::current();
    let km = s.ext.get::<urfin_keymap::ActiveKeymap>();
    SEARCH_KEYS
        .iter()
        .map(|&(cmd, key, label)| {
            let bound = km
                .filter(|_| !cmd.is_empty())
                .and_then(|k| k.keymap.keys_for(&Command::new(cmd), s).first().cloned());
            let label = if form && cmd.is_empty() {
                "ui-keys-run"
            } else {
                label
            };
            let key = bound.map_or_else(|| key.to_owned(), |c| c.to_string());
            (key, i18n.get(label))
        })
        .collect()
}

/// Заглушка keymap: пресет Far (S0-decisions №21) до S3-04.
#[derive(Debug, Default)]
pub struct FarStub;

impl KeyHints for FarStub {
    fn fkeys(&self, mods: Mods) -> [Option<FKey>; 12] {
        FAR[mods.idx()].map(|slot| {
            let (cmd, label) = slot?;
            Some(FKey {
                cmd: Command::parse(cmd).expect("stub command parses"),
                label_key: label.into(),
            })
        })
    }

    fn hint(&self, cmd: &Command) -> Option<CompactString> {
        let text = cmd.to_string();
        for mods in [Mods::None, Mods::Shift, Mods::Ctrl, Mods::Alt] {
            if let Some(n) = FAR[mods.idx()]
                .iter()
                .position(|s| s.is_some_and(|(c, _)| c == text))
            {
                return Some(format!("{}F{}", mods.prefix(), n + 1).into());
            }
        }
        FAR_EXTRA
            .iter()
            .find(|(c, _)| *c == text)
            .map(|(_, k)| CompactString::from(*k))
    }
}

impl FarStub {
    /// Все привязки заглушки: команда и сочетание (для реестра команд).
    pub fn bindings() -> Vec<(Command, String)> {
        let mut out = Vec::new();
        for mods in [Mods::None, Mods::Shift, Mods::Ctrl, Mods::Alt] {
            for (n, slot) in FAR[mods.idx()].iter().enumerate() {
                if let Some((cmd, _)) = slot {
                    let c = Command::parse(cmd).expect("stub command parses");
                    out.push((c, format!("{}F{}", mods.prefix(), n + 1)));
                }
            }
        }
        for (cmd, key) in FAR_EXTRA {
            out.push((
                Command::parse(cmd).expect("stub command parses"),
                (*key).to_owned(),
            ));
        }
        out
    }
}

/// Fluent-ключ команды: точки id → дефисы (`ops.copy` → `ops-copy`).
pub fn key_part(id: &str) -> String {
    id.replace('.', "-")
}

/// Подпись F-клавиши: `label_key`, иначе `cmd-<id>`, иначе id.
pub fn label(i18n: &I18n, label_key: &str, id: &str) -> String {
    let fallback = format!("cmd-{}", key_part(id));
    for key in [label_key, fallback.as_str()] {
        if i18n.has(key) {
            return i18n.get(key);
        }
    }
    id.to_owned()
}

/// Подготовленная строка: подписи всех наборов модификаторов пересчитываются при
/// смене языка (`epoch`), не каждый кадр.
#[derive(Debug, Default)]
pub struct Keybar {
    epoch: Option<u64>,
    rows: [Vec<Item>; 4],
}

/// F-клавиша строки.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// 1…10.
    pub n: u8,
    pub cmd: Command,
    pub label: String,
    /// Команда зарегистрирована (иначе — серая, без клика).
    pub enabled: bool,
}

impl Keybar {
    /// Обновить подписи, если сменился язык.
    pub fn refresh(
        &mut self,
        i18n: &I18n,
        epoch: u64,
        registry: &[&'static str],
        hints: &dyn KeyHints,
    ) {
        if self.epoch == Some(epoch) {
            return;
        }
        self.epoch = Some(epoch);
        for mods in [Mods::None, Mods::Shift, Mods::Ctrl, Mods::Alt] {
            self.rows[mods.idx()] = hints
                .fkeys(mods)
                .into_iter()
                .zip(1u8..=SLOTS)
                .filter_map(|(slot, n)| {
                    let FKey { cmd, label_key } = slot?;
                    let id = cmd.id.as_str();
                    Some(Item {
                        n,
                        label: label(i18n, &label_key, id),
                        enabled: registry.contains(&id),
                        cmd,
                    })
                })
                .collect();
        }
    }

    /// Строка для зажатых модификаторов.
    pub fn items(&self, mods: Mods) -> &[Item] {
        &self.rows[mods.idx()]
    }
}

/// Id зарегистрированных команд.
pub fn registered(registry: &CommandRegistry) -> Vec<&'static str> {
    registry.iter().map(|m| m.id).collect()
}

#[cfg(test)]
mod tests {
    use super::{FarStub, search_keys};

    /// Подписи `screen-search` по порядку макета; Enter в форме — запуск.
    #[test]
    fn search_screen_keys_follow_the_mockup() {
        urfin_i18n::set_language("ru");
        let tab =
            |id| urfin_core::Tab::new(urfin_core::TabId(id), urfin_core::Location::local("/a"));
        let s = urfin_core::AppState::new(
            urfin_core::Panel::new(tab(1)),
            urfin_core::Panel::new(tab(2)),
        );
        let keys = search_keys(&s, false);
        let want = [
            ("Enter", "Перейти"),
            ("F3", "Смотр"),
            ("F4", "Ред"),
            ("Ctrl+Enter", "В панель"),
            ("Alt+F7", "Новый поиск"),
            ("Esc", "Закрыть"),
        ];
        let got: Vec<_> = keys.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        assert_eq!(got, want);
        assert_eq!(search_keys(&s, true)[0].1, "Выполнить");
    }

    /// У заглушки одно сочетание — одна команда (конфликты пользовательских keymap
    /// ловит компиляция keymap S3, §2.5 `Ambiguous`).
    #[test]
    fn stub_keys_are_unique() {
        let b = FarStub::bindings();
        let mut keys: Vec<&str> = b.iter().map(|(_, k)| k.as_str()).collect();
        keys.sort_unstable();
        let mut dups = keys.clone();
        dups.dedup();
        assert_eq!(keys.len(), dups.len(), "{keys:?}");
    }
}
