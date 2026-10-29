//! Каталог ресурсов (вшитые + `config_dir/i18n/<lang>/`) и цепочка бандлов языка.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentError, FluentResource};
use unic_langid::LanguageIdentifier;

use crate::fmt::{NumFmt, formatter};

type Bundle = FluentBundle<Arc<FluentResource>>;

/// Язык по умолчанию и последнее звено цепочки.
pub const FALLBACK: &str = "en";

/// Вшитые ресурсы: (язык, файл, текст). Каждый файл `locales/` должен быть здесь (lint).
/// Строки интерфейса — файл на часть экрана (`ui-panel`, `ui-strip`, …): правки разных
/// частей не сталкиваются (скилл pets-dev).
pub const EMBEDDED: &[(&str, &str, &str)] = &[
    ("en", "cmd.ftl", include_str!("../locales/en/cmd.ftl")),
    ("en", "core.ftl", include_str!("../locales/en/core.ftl")),
    ("en", "fmt.ftl", include_str!("../locales/en/fmt.ftl")),
    ("en", "keybar.ftl", include_str!("../locales/en/keybar.ftl")),
    ("en", "keymap.ftl", include_str!("../locales/en/keymap.ftl")),
    ("en", "net.ftl", include_str!("../locales/en/net.ftl")),
    ("en", "pkg.ftl", include_str!("../locales/en/pkg.ftl")),
    (
        "en",
        "ui-jobs.ftl",
        include_str!("../locales/en/ui-jobs.ftl"),
    ),
    (
        "en",
        "ui-menu.ftl",
        include_str!("../locales/en/ui-menu.ftl"),
    ),
    (
        "en",
        "ui-panel.ftl",
        include_str!("../locales/en/ui-panel.ftl"),
    ),
    (
        "en",
        "ui-settings.ftl",
        include_str!("../locales/en/ui-settings.ftl"),
    ),
    (
        "en",
        "ui-strip.ftl",
        include_str!("../locales/en/ui-strip.ftl"),
    ),
    (
        "en",
        "ui-window.ftl",
        include_str!("../locales/en/ui-window.ftl"),
    ),
    ("ru", "cmd.ftl", include_str!("../locales/ru/cmd.ftl")),
    ("ru", "core.ftl", include_str!("../locales/ru/core.ftl")),
    ("ru", "fmt.ftl", include_str!("../locales/ru/fmt.ftl")),
    ("ru", "keybar.ftl", include_str!("../locales/ru/keybar.ftl")),
    ("ru", "keymap.ftl", include_str!("../locales/ru/keymap.ftl")),
    ("ru", "net.ftl", include_str!("../locales/ru/net.ftl")),
    ("ru", "pkg.ftl", include_str!("../locales/ru/pkg.ftl")),
    (
        "ru",
        "ui-jobs.ftl",
        include_str!("../locales/ru/ui-jobs.ftl"),
    ),
    (
        "ru",
        "ui-menu.ftl",
        include_str!("../locales/ru/ui-menu.ftl"),
    ),
    (
        "ru",
        "ui-panel.ftl",
        include_str!("../locales/ru/ui-panel.ftl"),
    ),
    (
        "ru",
        "ui-settings.ftl",
        include_str!("../locales/ru/ui-settings.ftl"),
    ),
    (
        "ru",
        "ui-strip.ftl",
        include_str!("../locales/ru/ui-strip.ftl"),
    ),
    (
        "ru",
        "ui-window.ftl",
        include_str!("../locales/ru/ui-window.ftl"),
    ),
];

/// Неприятность при загрузке ресурсов. Не фатальна: битые сообщения пропускаются,
/// остальное работает.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum I18nWarning {
    #[error("{file}:{line}: {message}")]
    Parse {
        file: PathBuf,
        line: usize,
        message: String,
    },
    #[error("{path}: {message}")]
    Io { path: PathBuf, message: String },
    #[error("{path}: not a language tag")]
    BadLanguage { path: PathBuf },
    #[error("{lang}: {message}")]
    Bundle { lang: String, message: String },
    #[error("language {requested:?} is not available, using {used}")]
    UnknownLanguage { requested: String, used: String },
}

/// Ресурс одного файла.
#[derive(Debug)]
struct Source {
    resource: Arc<FluentResource>,
    /// Пользовательский файл переопределяет вшитые сообщения.
    user: bool,
}

/// Все известные ресурсы по языкам. Загрузка — единственное место с I/O.
#[derive(Debug, Default)]
pub struct Catalog {
    langs: BTreeMap<String, (LanguageIdentifier, Vec<Source>)>,
}

impl Catalog {
    /// Только вшитые en/ru.
    pub fn embedded() -> Self {
        let mut catalog = Self::default();
        for &(lang, file, text) in EMBEDDED {
            let path = Path::new(lang).join(file);
            let resource = parse(text.to_owned(), &path, &mut Vec::new());
            catalog.push(
                lang.parse().expect("embedded language tag"),
                resource,
                false,
            );
        }
        catalog
    }

    /// Вшитые + `<config_dir>/i18n/<lang>/*.ftl` (файлы по имени). Нет каталога — не ошибка.
    pub fn load(config_dir: Option<&Path>) -> (Self, Vec<I18nWarning>) {
        let mut catalog = Self::embedded();
        let mut warnings = Vec::new();
        if let Some(dir) = config_dir {
            catalog.load_user(&dir.join("i18n"), &mut warnings);
        }
        (catalog, warnings)
    }

    fn load_user(&mut self, root: &Path, warnings: &mut Vec<I18nWarning>) {
        let io = |path: &Path, err: std::io::Error| I18nWarning::Io {
            path: path.to_owned(),
            message: err.to_string(),
        };
        let mut dirs = match read_dir_sorted(root) {
            Ok(dirs) => dirs,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
            Err(err) => return warnings.push(io(root, err)),
        };
        dirs.retain(|p| p.is_dir());
        for dir in dirs {
            let tag = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            let Ok(lang) = tag.parse::<LanguageIdentifier>() else {
                warnings.push(I18nWarning::BadLanguage { path: dir.clone() });
                continue;
            };
            let files = match read_dir_sorted(&dir) {
                Ok(files) => files,
                Err(err) => {
                    warnings.push(io(&dir, err));
                    continue;
                }
            };
            for file in files {
                if file.extension().is_none_or(|e| e != "ftl") || !file.is_file() {
                    continue;
                }
                match std::fs::read_to_string(&file) {
                    Ok(text) => {
                        let resource = parse(text, &file, warnings);
                        self.push(lang.clone(), resource, true);
                    }
                    Err(err) => warnings.push(io(&file, err)),
                }
            }
        }
    }

    fn push(&mut self, lang: LanguageIdentifier, resource: Arc<FluentResource>, user: bool) {
        let key = lang.to_string();
        let entry = self.langs.entry(key).or_insert_with(|| (lang, Vec::new()));
        entry.1.push(Source { resource, user });
    }

    /// Есть ли сообщение в ресурсах самого языка, без фолбэка на en (линты; собирает
    /// бандл на каждый вызов).
    pub fn has_own(&self, lang: &LanguageIdentifier, key: &str) -> bool {
        self.bundle(lang, NumFmt::of(lang), &mut Vec::new())
            .has_message(key)
    }

    /// Языки, для которых есть ресурсы, по тегу.
    pub fn available(&self) -> Vec<LanguageIdentifier> {
        self.langs.values().map(|(lang, _)| lang.clone()).collect()
    }

    /// Язык каталога для запроса: `auto`/пусто — язык ОС; точное совпадение тега, затем
    /// по языковому подтегу; иначе en с предупреждением.
    pub fn resolve(&self, requested: &str) -> (LanguageIdentifier, Option<I18nWarning>) {
        let requested = requested.trim();
        let wanted = if requested.is_empty() || requested.eq_ignore_ascii_case("auto") {
            sys_locale::get_locale().unwrap_or_else(|| FALLBACK.to_owned())
        } else {
            requested.to_owned()
        };
        if let Some(lang) = self.lookup(&wanted) {
            return (lang, None);
        }
        let fallback: LanguageIdentifier = FALLBACK.parse().expect("fallback tag");
        // Язык ОС, которого нет, — не повод для предупреждения.
        let warning =
            (!requested.is_empty() && !requested.eq_ignore_ascii_case("auto")).then(|| {
                I18nWarning::UnknownLanguage {
                    requested: requested.to_owned(),
                    used: FALLBACK.to_owned(),
                }
            });
        (fallback, warning)
    }

    fn lookup(&self, wanted: &str) -> Option<LanguageIdentifier> {
        // sys-locale на Unix может дать `ru_RU.UTF-8`.
        let tag = wanted
            .split(['.', '@'])
            .next()
            .unwrap_or_default()
            .replace('_', "-");
        let wanted: LanguageIdentifier = tag.parse().ok()?;
        let langs = self.langs.values().map(|(lang, _)| lang);
        if let Some(exact) = langs.clone().find(|l| **l == wanted) {
            return Some(exact.clone());
        }
        let same = |l: &&LanguageIdentifier| l.language == wanted.language;
        // Сначала язык без региона (`ru` для `ru-RU`), затем любой с тем же языком.
        langs
            .clone()
            .find(|l| same(l) && l.region.is_none())
            .or_else(|| langs.clone().find(same))
            .cloned()
    }

    /// Цепочка бандлов для языка: `[lang, en]` (или `[en]`).
    pub fn build(&self, lang: &LanguageIdentifier) -> (I18n, Vec<I18nWarning>) {
        let mut warnings = Vec::new();
        // Числа — по языку интерфейса во всей цепочке, плюрали — по языку сообщения.
        let num_fmt = NumFmt::of(lang);
        let mut chain = vec![self.bundle(lang, num_fmt, &mut warnings)];
        let fallback: LanguageIdentifier = FALLBACK.parse().expect("fallback tag");
        if *lang != fallback {
            chain.push(self.bundle(&fallback, num_fmt, &mut warnings));
        }
        let i18n = I18n {
            lang: lang.clone(),
            chain,
            num_fmt,
            warned: Mutex::default(),
        };
        (i18n, warnings)
    }

    fn bundle(
        &self,
        lang: &LanguageIdentifier,
        num_fmt: NumFmt,
        warnings: &mut Vec<I18nWarning>,
    ) -> Bundle {
        let mut bundle = Bundle::new_concurrent(vec![lang.clone()]);
        // Иначе U+2068/2069 вокруг аргументов видны в egui (S2 §7).
        bundle.set_use_isolating(false);
        bundle.set_formatter(Some(formatter(num_fmt)));
        let sources = self
            .langs
            .get(&lang.to_string())
            .map_or(&[][..], |(_, s)| s);
        for source in sources {
            if source.user {
                bundle.add_resource_overriding(Arc::clone(&source.resource));
            } else if let Err(errors) = bundle.add_resource(Arc::clone(&source.resource)) {
                for err in errors {
                    warnings.push(bundle_warning(lang, &err));
                }
            }
        }
        bundle
    }
}

fn bundle_warning(lang: &LanguageIdentifier, err: &FluentError) -> I18nWarning {
    I18nWarning::Bundle {
        lang: lang.to_string(),
        message: err.to_string(),
    }
}

fn read_dir_sorted(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

/// Разбор с сохранением целых сообщений: битые записи пропускаются с предупреждением.
fn parse(text: String, file: &Path, warnings: &mut Vec<I18nWarning>) -> Arc<FluentResource> {
    match FluentResource::try_new(text) {
        Ok(resource) => Arc::new(resource),
        Err((resource, errors)) => {
            for err in errors {
                // Строка начала битой записи, а не места, где это заметил парсер.
                let at = err.slice.as_ref().map_or(err.pos.start, |s| s.start);
                let source = resource.source();
                let line = source[..at.min(source.len())].matches('\n').count() + 1;
                warnings.push(I18nWarning::Parse {
                    file: file.to_owned(),
                    line,
                    message: err.to_string(),
                });
            }
            Arc::new(resource)
        }
    }
}

/// Ключи с уже выданным предупреждением.
#[derive(Debug, Default)]
struct Warned {
    missing: HashSet<String>,
    /// Ошибки подстановки (нет аргумента и т.п.).
    broken: HashSet<String>,
}

/// Тексты одного языка: цепочка `[язык, en]`, затем `⟨key⟩`.
pub struct I18n {
    lang: LanguageIdentifier,
    chain: Vec<Bundle>,
    num_fmt: NumFmt,
    /// Ключи, о которых уже предупредили (один warn на ключ).
    warned: Mutex<Warned>,
}

impl std::fmt::Debug for I18n {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("I18n")
            .field("lang", &self.lang)
            .field("chain", &self.chain.len())
            .finish_non_exhaustive()
    }
}

impl I18n {
    /// Язык интерфейса.
    pub fn lang(&self) -> &LanguageIdentifier {
        &self.lang
    }

    /// Разделители чисел языка.
    pub fn num_fmt(&self) -> NumFmt {
        self.num_fmt
    }

    /// Есть ли сообщение в цепочке.
    pub fn has(&self, key: &str) -> bool {
        self.chain.iter().any(|b| b.has_message(key))
    }

    /// Текст сообщения без аргументов.
    pub fn get(&self, key: &str) -> String {
        self.format(key, None, None)
    }

    /// Текст атрибута (`cmd-ops-copy.menu-key`).
    pub fn attr(&self, key: &str, attr: &str) -> String {
        self.format(key, Some(attr), None)
    }

    /// Текст сообщения с аргументами.
    pub fn fmt(&self, key: &str, args: &FluentArgs) -> String {
        self.format(key, None, Some(args))
    }

    /// Ключи, которых не нашлось ни в одном бандле (диагностика, линты S2-02).
    pub fn missing_keys(&self) -> Vec<String> {
        let warned = self.warned.lock().unwrap_or_else(PoisonError::into_inner);
        let mut keys: Vec<_> = warned.missing.iter().cloned().collect();
        keys.sort();
        keys
    }

    fn format(&self, key: &str, attr: Option<&str>, args: Option<&FluentArgs>) -> String {
        for bundle in &self.chain {
            let Some(message) = bundle.get_message(key) else {
                continue;
            };
            let pattern = match attr {
                Some(name) => message.get_attribute(name).map(|a| a.value()),
                None => message.value(),
            };
            let Some(pattern) = pattern else {
                continue;
            };
            let mut errors = Vec::new();
            let text = bundle
                .format_pattern(pattern, args, &mut errors)
                .into_owned();
            if !errors.is_empty() {
                self.warn_once(false, key, attr, || format!("{errors:?}"));
            }
            return text;
        }
        self.warn_once(true, key, attr, || "missing".to_owned());
        match attr {
            Some(a) => format!("⟨{key}.{a}⟩"),
            None => format!("⟨{key}⟩"),
        }
    }

    /// Ошибка подстановки сообщения вне Fluent (шаблон даты и т.п.): один warn на ключ.
    pub(crate) fn warn_broken(&self, key: &str, detail: impl FnOnce() -> String) {
        self.warn_once(false, key, None, detail);
    }

    fn warn_once(
        &self,
        missing: bool,
        key: &str,
        attr: Option<&str>,
        detail: impl FnOnce() -> String,
    ) {
        let full = attr.map_or_else(|| key.to_owned(), |a| format!("{key}.{a}"));
        let mut warned = self.warned.lock().unwrap_or_else(PoisonError::into_inner);
        let set = if missing {
            &mut warned.missing
        } else {
            &mut warned.broken
        };
        let fresh = set.insert(full.clone());
        drop(warned);
        if fresh {
            tracing::warn!(key = %full, lang = %self.lang, detail = %detail(), "i18n");
        }
    }
}
