//! Запись настроек из UI в `config.toml` (S2-22, S0-decisions №1): ключ меняется через
//! `toml_edit`, комментарии, порядок и оформление файла остаются как были.
//!
//! [`set`] копит изменения в состоянии; Core пишет их одной записью через [`DEBOUNCE`]
//! после последнего изменения — на `Lane::Bulk` (S0 v2 №7), атомарно (временный файл
//! рядом + rename). Перед `app.reload_config` и при выходе отложенное пишется сразу.
//! Битый конфиг и конфиг будущей версии не перезаписываются — тост.
//!
//! `AppState.config` запись не меняет: это снимок файла с последней загрузки, а
//! действующие значения — у владельцев (`UiState`, раскладка keymap). После
//! `app.reload_config` они читаются из файла, где уже есть всё записанное.

use std::fs;
use std::io;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use compact_str::CompactString;
use toml_edit::{DocumentMut, Item, Table, TableLike};

use crate::config::{CONFIG_VERSION, DEFAULT_TEMPLATE};
use crate::msg::{Msg, msg};
use crate::paths::write_atomic;
use crate::state::Level;
use crate::text::tr;

/// Значение ключа; `&str`, `String`, `i64`, `f64`, `bool` приводятся сами.
pub use toml_edit::Value;

/// Запись — через столько после последнего изменения (S2 §9, S2-22).
pub const DEBOUNCE: Duration = Duration::from_secs(1);

/// Путь ключа (`["ui", "theme"]`) и значение.
pub(crate) type Change = (Vec<CompactString>, Value);

/// Записать `value` в ключ `path` конфига (`&["ui", "theme"]` — `[ui] theme`). Нет
/// таблицы — создаётся в конце файла; нет файла — пишется шаблон с этим ключом.
///
/// Возвращает `Msg`: в handler — `settings::set(…).apply(cx)`, из задачи — через
/// `sink`. Значение не проверяется: тип и диапазон — забота вызывающего.
pub fn set(path: &[&str], value: impl Into<Value>) -> Box<dyn Msg> {
    let path: Vec<CompactString> = path.iter().map(|&p| p.into()).collect();
    let value = value.into();
    msg("settings.set", move |cx| {
        if path.is_empty() {
            debug_assert!(false, "settings::set: empty path");
            return;
        }
        cx.state.settings.push(path, value);
        cx.rt.waker().wake_after(DEBOUNCE);
    })
}

/// Изменения, ждущие записи (в `AppState`; пишет Core в конце pump).
#[derive(Debug, Default)]
pub(crate) struct Pending {
    changes: Vec<Change>,
    /// Время последнего изменения.
    pub(crate) changed: Option<Instant>,
}

impl Pending {
    /// Повтор ключа заменяет прежнее значение.
    fn push(&mut self, path: Vec<CompactString>, value: Value) {
        self.changes.retain(|(p, _)| *p != path);
        self.changes.push((path, value));
        self.changed = Some(Instant::now());
    }

    pub(crate) fn take(&mut self) -> Vec<Change> {
        self.changed = None;
        mem::take(&mut self.changes)
    }
}

/// Почему настройки не записаны.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SettingsError {
    /// Файл не разбирается: не трогаем, пользователь правит сам.
    #[error("the file has errors: {0}")]
    Broken(String),
    #[error("version {0} is newer than {CONFIG_VERSION}: the file is left untouched")]
    Future(i64),
    /// Путь к ключу занят не таблицей (`ui = 1`); остальные ключи записаны.
    #[error("'{0}' is not a table")]
    Conflict(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Запись файла настроек: общая для задач записи, `app.reload_config` и выхода.
#[derive(Debug)]
pub(crate) struct Writer {
    file: PathBuf,
    /// Изменения к записи; держится коротко — сюда кладёт UI-поток.
    queue: Mutex<Vec<Change>>,
    /// Держится всю запись: задачи пишут по одной, каждая — всё, что в очереди.
    io: Mutex<()>,
}

impl Writer {
    pub(crate) fn new(file: PathBuf) -> Self {
        Writer {
            file,
            queue: Mutex::default(),
            io: Mutex::default(),
        }
    }

    pub(crate) fn file(&self) -> &Path {
        &self.file
    }

    /// UI-поток: изменения — в очередь (без I/O). Задача записи, запущенная после,
    /// их увидит.
    pub(crate) fn enqueue(&self, mut changes: Vec<Change>) {
        if changes.is_empty() {
            return;
        }
        let mut q = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        for (path, _) in &changes {
            q.retain(|(p, _)| p != path);
        }
        q.append(&mut changes);
    }

    /// Не в UI-потоке: всё из очереди — в файл. `Ok(false)` — писать нечего или файл
    /// не изменился бы.
    pub(crate) fn flush(&self) -> Result<bool, SettingsError> {
        self.flush_then(|| ()).0
    }

    /// Запись, затем `after` — не отпуская файл (перечитать конфиг после записи).
    pub(crate) fn flush_then<R>(
        &self,
        after: impl FnOnce() -> R,
    ) -> (Result<bool, SettingsError>, R) {
        let _io = self.io.lock().unwrap_or_else(PoisonError::into_inner);
        let changes = {
            let mut q = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
            mem::take(&mut *q)
        };
        let saved = if changes.is_empty() {
            Ok(false)
        } else {
            write(&self.file, &changes)
        };
        let file = self.file.display();
        match &saved {
            Ok(true) => tracing::debug!(%file, keys = changes.len(), "settings saved"),
            Ok(false) => {}
            Err(e) => tracing::error!(%file, error = %e, "settings not saved"),
        }
        (saved, after())
    }
}

/// Тост «настройки не сохранены».
pub(crate) fn failed(file: &Path, e: &SettingsError) -> Box<dyn Msg> {
    let text = tr(
        "core-settings-not-saved",
        &[
            ("file", &file.display().to_string()),
            ("error", &e.to_string()),
        ],
    );
    msg("settings.failed", move |cx| cx.toast(Level::Warn, text))
}

/// Прочитать, изменить, записать атомарно. Ключи с конфликтом пропускаются, остальные
/// пишутся; первый конфликт — ошибкой после записи.
fn write(file: &Path, changes: &[Change]) -> Result<bool, SettingsError> {
    let (text, existed) = match fs::read_to_string(file) {
        Ok(t) => (t, true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => (DEFAULT_TEMPLATE.to_owned(), false),
        Err(e) => return Err(e.into()),
    };
    let (new, conflict) = edit(&text, changes)?;
    let written = !existed || new != text;
    if written {
        write_atomic(file, new.as_bytes())?;
    }
    match conflict {
        Some(key) => Err(SettingsError::Conflict(key)),
        None => Ok(written),
    }
}

/// Текст конфига с изменениями и первый ключ, который поставить нельзя.
pub(crate) fn edit(
    text: &str,
    changes: &[Change],
) -> Result<(String, Option<String>), SettingsError> {
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| SettingsError::Broken(e.message().to_owned()))?;
    if let Some(v) = doc.get("version").and_then(Item::as_integer)
        && v > i64::from(CONFIG_VERSION)
    {
        return Err(SettingsError::Future(v));
    }
    let mut conflict = None;
    for (path, value) in changes {
        if let Err(at) = set_path(&mut doc, path, value) {
            conflict.get_or_insert(at);
        }
    }
    Ok((doc.to_string(), conflict))
}

/// Поставить значение; прежнее оформление (`theme = "x"   # комментарий`) остаётся.
/// `Err` — путь (до места конфликта), занятый не таблицей.
fn set_path(doc: &mut DocumentMut, path: &[CompactString], value: &Value) -> Result<(), String> {
    let Some((key, parents)) = path.split_last() else {
        return Ok(());
    };
    let at = |n: usize| path[..n].join(".");
    let mut table: &mut dyn TableLike = doc.as_table_mut();
    for (i, p) in parents.iter().enumerate() {
        let item = table.entry(p).or_insert_with(|| {
            let mut t = Table::new();
            // Промежуточные таблицы без своего заголовка: `[a.b]`, а не `[a]` + `[a.b]`.
            t.set_implicit(i + 1 < parents.len());
            Item::Table(t)
        });
        table = item.as_table_like_mut().ok_or_else(|| at(i + 1))?;
    }
    match table.get_mut(key) {
        Some(Item::Value(old)) => {
            let decor = old.decor().clone();
            *old = value.clone();
            *old.decor_mut() = decor;
        }
        None | Some(Item::None) => {
            table.insert(key, Item::Value(value.clone()));
        }
        Some(Item::Table(_) | Item::ArrayOfTables(_)) => return Err(at(path.len())),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Change, SettingsError, Value, Writer, edit};
    use crate::config::{Config, DEFAULT_TEMPLATE};
    use crate::paths::Paths;
    use std::fs;

    fn ch(path: &[&str], v: impl Into<Value>) -> Change {
        (path.iter().map(|&p| p.into()).collect(), v.into())
    }

    const COMMENTED: &str = r##"# Мой конфиг: комментарии должны остаться.
version = 1

[general]
# Язык интерфейса.
language = "auto"   # auto | en | ru

[ui]
theme = "system"          # system | light | dark
accent = "#0067C0"
# Масштаб — ниже.
zoom = 100
[ui.tokens.light]         # точечные переопределения
"accent.fill" = "#0067C0"

[log]
level = "info"
"##;

    #[test]
    fn keeps_comments_order_and_formatting() {
        let changes = [
            ch(&["ui", "theme"], "dark"),
            ch(&["ui", "zoom"], 120),
            ch(&["ui", "split"], 0.6),
            ch(&["general", "language"], "ru"),
            ch(&["keymap", "preset"], "dn"),
        ];
        let (text, conflict) = edit(COMMENTED, &changes).unwrap();
        assert_eq!(conflict, None);
        insta::assert_snapshot!("settings_edit_commented", text);
        // Изменённое читается загрузчиком.
        let tmp = tempfile::tempdir().unwrap();
        let p = Paths::in_dir(tmp.path());
        fs::write(p.config_file(), &text).unwrap();
        let (c, w) = Config::load(&p);
        assert_eq!(w, vec![]);
        assert_eq!(c.general.language, "ru");
        let ui = c.ext["ui"].as_table().unwrap();
        assert_eq!(ui["theme"].as_str(), Some("dark"));
        assert_eq!(ui["zoom"].as_integer(), Some(120));
        assert_eq!(ui["accent"].as_str(), Some("#0067C0"));
        assert_eq!(c.ext["keymap"]["preset"].as_str(), Some("dn"));
    }

    #[test]
    fn same_value_changes_nothing() {
        let (text, _) = edit(COMMENTED, &[ch(&["ui", "theme"], "system")]).unwrap();
        assert_eq!(text, COMMENTED);
    }

    #[test]
    fn dotted_and_inline_tables() {
        let text = "ui.theme = \"light\"\nkeymap = { preset = \"far\" }\n";
        let changes = [
            ch(&["ui", "theme"], "dark"),
            ch(&["keymap", "preset"], "dn"),
        ];
        let (new, conflict) = edit(text, &changes).unwrap();
        assert_eq!(conflict, None);
        assert_eq!(new, "ui.theme = \"dark\"\nkeymap = { preset = \"dn\" }\n");
    }

    #[test]
    fn conflict_skips_only_that_key() {
        let text = "ui = 1\n[general]\nlanguage = \"auto\"\n";
        let changes = [
            ch(&["ui", "theme"], "dark"),
            ch(&["general", "language"], "ru"),
        ];
        let (new, conflict) = edit(text, &changes).unwrap();
        assert_eq!(conflict.as_deref(), Some("ui"));
        assert_eq!(new, "ui = 1\n[general]\nlanguage = \"ru\"\n");
        // Таблица на месте значения — тоже конфликт.
        let (_, conflict) = edit("[ui.theme]\nx = 1\n", &[ch(&["ui", "theme"], "dark")]).unwrap();
        assert_eq!(conflict.as_deref(), Some("ui.theme"));
    }

    fn writer() -> (tempfile::TempDir, Paths, Writer) {
        let tmp = tempfile::tempdir().unwrap();
        let p = Paths::in_dir(tmp.path().join("cfg"));
        let w = Writer::new(p.config_file());
        (tmp, p, w)
    }

    #[test]
    fn broken_or_future_config_is_not_overwritten() {
        let (_t, p, w) = writer();
        fs::create_dir_all(&p.config_dir).unwrap();
        for bad in [
            "version = = 1\n[ui\n",
            "version = 7\n[ui]\ntheme = \"light\"\n",
        ] {
            fs::write(p.config_file(), bad).unwrap();
            w.enqueue(vec![ch(&["ui", "theme"], "dark")]);
            let e = w.flush().unwrap_err();
            assert!(
                matches!(e, SettingsError::Broken(_) | SettingsError::Future(7)),
                "{e:?}"
            );
            assert_eq!(fs::read_to_string(p.config_file()).unwrap(), bad);
            assert_eq!(
                fs::read_dir(&p.config_dir).unwrap().count(),
                1,
                "no temp files"
            );
        }
    }

    #[test]
    fn missing_file_gets_template_with_key() {
        let (_t, p, w) = writer();
        assert!(!w.flush().unwrap(), "empty queue writes nothing");
        assert!(!p.config_file().exists());
        w.enqueue(vec![ch(&["ui", "theme"], "light")]);
        w.enqueue(vec![ch(&["ui", "theme"], "dark")]);
        assert!(w.flush().unwrap());
        let text = fs::read_to_string(p.config_file()).unwrap();
        assert!(text.starts_with(&DEFAULT_TEMPLATE[..40]), "{text}");
        assert!(text.ends_with("[ui]\ntheme = \"dark\"\n"), "{text}");
        let (c, w2) = Config::load(&p);
        assert_eq!(w2, vec![]);
        assert_eq!(c.ext["ui"]["theme"].as_str(), Some("dark"));
        // Повтор того же значения файл не трогает.
        w.enqueue(vec![ch(&["ui", "theme"], "dark")]);
        assert!(!w.flush().unwrap());
    }
}
