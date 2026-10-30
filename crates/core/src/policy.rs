//! Политики организации (ADR-0020, F00-01): `policy.toml` (TOML) и
//! `HKLM\Software\Policies\Aahz\Urfin` (GPO), объединённые в памяти в [`PolicySet`].
//!
//! Здесь только модель и разбор уже прочитанных данных: файл и реестр читает
//! вызывающий. Ключ задан в обоих источниках — побеждает GPO. Отсутствующий или
//! неверный ключ — «не задано», действует настройка пользователя (неверный — с
//! [`PolicyError`]).

use std::collections::BTreeMap;
use std::fmt;

use compact_str::CompactString;

/// Откуда значение политики; подписывается в разделе «Политики».
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PolicySource {
    Toml,
    Gpo,
}

impl PolicySource {
    /// Подпись источника: `TOML` или `GPO` (не переводится).
    pub fn label(self) -> &'static str {
        match self {
            PolicySource::Toml => "TOML",
            PolicySource::Gpo => "GPO",
        }
    }
}

impl fmt::Display for PolicySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Значение из реестра, как его прочитал вызывающий.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GpoValue {
    Dword(u32),
    Sz(String),
    /// Значение есть, но другого типа.
    Other,
}

/// Почему значение политики не принято; остальные ключи действуют.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    /// `policy.toml` не разбирается: из файла не принято ничего.
    #[error("policy.toml: {0}")]
    Toml(String),
    #[error("TOML {0}: unknown key")]
    UnknownKey(String),
    /// Неверный тип или значение; `name` — ключ TOML или имя значения реестра.
    #[error("{from} {name}: {expected}")]
    BadValue {
        from: PolicySource,
        name: &'static str,
        expected: &'static str,
    },
}

/// Ключ политики (таблица ключей ADR-0020).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PolicyKey {
    Theme,
    ThemeLocked,
    DeleteToTrash,
    ExperimentalFastTrash,
    Verify,
    /// Только читается и показывается до принятия ADR-0021.
    DuElevatedHelper,
    /// Только читается и показывается до принятия ADR-0021.
    DuElevatedMft,
    PasswordCache,
    SshRecording,
    SshMultiExec,
}

/// Тип значения ключа.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Kind {
    /// TOML bool, `REG_DWORD` (≠ 0 — true).
    Bool,
    /// Непустая строка, `REG_SZ`.
    Theme,
    Verify,
    SshRecording,
}

impl PolicyKey {
    pub const ALL: [PolicyKey; 10] = [
        PolicyKey::Theme,
        PolicyKey::ThemeLocked,
        PolicyKey::DeleteToTrash,
        PolicyKey::ExperimentalFastTrash,
        PolicyKey::Verify,
        PolicyKey::DuElevatedHelper,
        PolicyKey::DuElevatedMft,
        PolicyKey::PasswordCache,
        PolicyKey::SshRecording,
        PolicyKey::SshMultiExec,
    ];

    /// Ключ в `policy.toml`; вложенные — через точку (`ssh.recording`).
    pub fn toml(self) -> &'static str {
        match self {
            PolicyKey::Theme => "theme",
            PolicyKey::ThemeLocked => "theme_locked",
            PolicyKey::DeleteToTrash => "delete_to_trash",
            PolicyKey::ExperimentalFastTrash => "experimental_fast_trash",
            PolicyKey::Verify => "verify",
            PolicyKey::DuElevatedHelper => "du_elevated_helper",
            PolicyKey::DuElevatedMft => "du_elevated_mft",
            PolicyKey::PasswordCache => "password_cache",
            PolicyKey::SshRecording => "ssh.recording",
            PolicyKey::SshMultiExec => "ssh.multi_exec",
        }
    }

    /// Имя значения в `HKLM\Software\Policies\Aahz\Urfin`.
    pub fn registry(self) -> &'static str {
        match self {
            PolicyKey::Theme => "Theme",
            PolicyKey::ThemeLocked => "ThemeLocked",
            PolicyKey::DeleteToTrash => "DeleteToTrash",
            PolicyKey::ExperimentalFastTrash => "ExperimentalFastTrash",
            PolicyKey::Verify => "Verify",
            PolicyKey::DuElevatedHelper => "DuElevatedHelper",
            PolicyKey::DuElevatedMft => "DuElevatedMft",
            PolicyKey::PasswordCache => "PasswordCache",
            PolicyKey::SshRecording => "SshRecording",
            PolicyKey::SshMultiExec => "SshMultiExec",
        }
    }

    fn kind(self) -> Kind {
        match self {
            PolicyKey::Theme => Kind::Theme,
            PolicyKey::Verify => Kind::Verify,
            PolicyKey::SshRecording => Kind::SshRecording,
            _ => Kind::Bool,
        }
    }

    fn from_toml(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.toml() == name)
    }
}

/// Минимальная проверка после записи (`verify`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MinVerify {
    None,
    Size,
    Hash,
}

/// Запись SSH-сеансов (`ssh.recording`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SshRecording {
    Required,
    Forbidden,
    /// Решает пользователь.
    User,
}

/// Значение политики.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyValue {
    Bool(bool),
    Theme(CompactString),
    Verify(MinVerify),
    SshRecording(SshRecording),
}

impl fmt::Display for PolicyValue {
    /// Как в `policy.toml`: `true`, `size`, `required`, имя темы.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PolicyValue::Bool(true) => "true",
            PolicyValue::Bool(false) => "false",
            PolicyValue::Theme(t) => t,
            PolicyValue::Verify(MinVerify::None) => "none",
            PolicyValue::Verify(MinVerify::Size) => "size",
            PolicyValue::Verify(MinVerify::Hash) => "hash",
            PolicyValue::SshRecording(SshRecording::Required) => "required",
            PolicyValue::SshRecording(SshRecording::Forbidden) => "forbidden",
            PolicyValue::SshRecording(SshRecording::User) => "user",
        })
    }
}

/// Значение и его источник.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyEntry {
    pub value: PolicyValue,
    pub source: PolicySource,
}

/// Настройки конфига под политикой: путь ключа (как у [`crate::settings::set`]) и ключ
/// политики. `theme` — только тема по умолчанию: замок даёт `theme_locked = true`.
const SETTINGS: [(&[&str], PolicyKey); 4] = [
    (&["ui", "theme"], PolicyKey::ThemeLocked),
    (&["ops", "delete_to_trash"], PolicyKey::DeleteToTrash),
    (
        &["ops", "experimental_fast_trash"],
        PolicyKey::ExperimentalFastTrash,
    ),
    (&["ops", "defaults", "verify"], PolicyKey::Verify),
];

/// Действующие политики: TOML, поверх — GPO.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PolicySet {
    entries: BTreeMap<PolicyKey, PolicyEntry>,
}

impl PolicySet {
    /// Разобрать текст `policy.toml`. Битый файл — пустой набор и [`PolicyError::Toml`];
    /// неверный ключ пропускается.
    pub fn from_toml(text: &str) -> (Self, Vec<PolicyError>) {
        let mut set = PolicySet::default();
        let mut errors = Vec::new();
        match text.parse::<toml::Table>() {
            Ok(t) => set.add_toml_table(&t, "", &mut errors),
            Err(e) => errors.push(PolicyError::Toml(e.message().into())),
        }
        (set, errors)
    }

    fn add_toml_table(&mut self, table: &toml::Table, prefix: &str, errors: &mut Vec<PolicyError>) {
        for (name, v) in table {
            let name = format!("{prefix}{name}");
            if let toml::Value::Table(t) = v {
                self.add_toml_table(t, &format!("{name}."), errors);
                continue;
            }
            let Some(key) = PolicyKey::from_toml(&name) else {
                errors.push(PolicyError::UnknownKey(name));
                continue;
            };
            match toml_value(key, v) {
                Ok(Some(value)) => self.put(key, value, PolicySource::Toml),
                Ok(None) => {}
                Err(expected) => errors.push(PolicyError::BadValue {
                    from: PolicySource::Toml,
                    name: key.toml(),
                    expected,
                }),
            }
        }
    }

    /// Наложить значения GPO (имя в реестре — [`PolicyKey::registry`]): они побеждают
    /// TOML. Неверное значение пропускается, TOML по этому ключу остаётся.
    pub fn merge_gpo(
        &mut self,
        values: impl IntoIterator<Item = (PolicyKey, GpoValue)>,
    ) -> Vec<PolicyError> {
        let mut errors = Vec::new();
        for (key, raw) in values {
            match gpo_value(key, &raw) {
                Ok(Some(value)) => self.put(key, value, PolicySource::Gpo),
                Ok(None) => {}
                Err(expected) => errors.push(PolicyError::BadValue {
                    from: PolicySource::Gpo,
                    name: key.registry(),
                    expected,
                }),
            }
        }
        errors
    }

    fn put(&mut self, key: PolicyKey, value: PolicyValue, source: PolicySource) {
        self.entries.insert(key, PolicyEntry { value, source });
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, key: PolicyKey) -> Option<&PolicyEntry> {
        self.entries.get(&key)
    }

    /// Заданные политики в порядке [`PolicyKey::ALL`] — для раздела «Политики».
    pub fn iter(&self) -> impl Iterator<Item = (PolicyKey, &PolicyEntry)> {
        self.entries.iter().map(|(k, e)| (*k, e))
    }

    /// Значение булева ключа; не задан или ключ не булев — `None`.
    pub fn bool(&self, key: PolicyKey) -> Option<bool> {
        match self.get(key)?.value {
            PolicyValue::Bool(b) => Some(b),
            _ => None,
        }
    }

    /// Тема по умолчанию (`theme`).
    pub fn theme(&self) -> Option<&str> {
        match &self.get(PolicyKey::Theme)?.value {
            PolicyValue::Theme(t) => Some(t),
            _ => None,
        }
    }

    pub fn verify(&self) -> Option<MinVerify> {
        match self.get(PolicyKey::Verify)?.value {
            PolicyValue::Verify(v) => Some(v),
            _ => None,
        }
    }

    pub fn ssh_recording(&self) -> Option<SshRecording> {
        match self.get(PolicyKey::SshRecording)?.value {
            PolicyValue::SshRecording(r) => Some(r),
            _ => None,
        }
    }

    /// Наложить политику на конфиг: заданные ключи `[ops]` получают значение политики,
    /// `verify` поднимается до минимума. Действующие значения у владельцев (ops) берутся
    /// из конфига — так политика доходит до них без отдельного канала. Тему ведёт ui.
    pub fn apply(&self, config: &mut crate::Config) {
        for key in [PolicyKey::DeleteToTrash, PolicyKey::ExperimentalFastTrash] {
            if let Some(b) = self.bool(key) {
                let ops = table(&mut config.ext, "ops");
                ops.insert(key.toml().into(), toml::Value::Boolean(b));
            }
        }
        let Some(min) = self.verify().filter(|m| *m > MinVerify::None) else {
            return;
        };
        let defaults = table(table(&mut config.ext, "ops"), "defaults");
        if verify_rank(defaults.get("verify")) < min {
            let v = match min {
                MinVerify::None => toml::Value::String("Off".into()),
                MinVerify::Size => toml::Value::String("Size".into()),
                MinVerify::Hash => {
                    toml::Value::Table(toml::Table::from_iter([("Hash".into(), "Xxh3_128".into())]))
                }
            };
            defaults.insert("verify".into(), v);
        }
    }

    /// Задана ли политикой настройка конфига `path` (`&["ops", "delete_to_trash"]`):
    /// `Some(источник)` — в настройках замок «Задано политикой организации», из UI она
    /// не меняется. Тема под замком только при `theme_locked = true`.
    pub fn setting(&self, path: &[&str]) -> Option<PolicySource> {
        let (_, key) = SETTINGS.iter().find(|(p, _)| *p == path)?;
        let e = self.get(*key)?;
        (*key != PolicyKey::ThemeLocked || e.value == PolicyValue::Bool(true)).then_some(e.source)
    }
}

/// Таблица `name` в `t`; нет или не таблица — новая пустая.
fn table<'a>(t: &'a mut toml::Table, name: &str) -> &'a mut toml::Table {
    let v = t
        .entry(name)
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if !v.is_table() {
        *v = toml::Value::Table(toml::Table::new());
    }
    v.as_table_mut().expect("table above")
}

/// Уровень `ops.defaults.verify` из конфига (serde `Verify` ops): `"Off"`, `"Size"`,
/// `{ Hash = … }`; нет или не разбирается — `None` (по умолчанию выкл).
fn verify_rank(v: Option<&toml::Value>) -> MinVerify {
    match v {
        Some(toml::Value::String(s)) if s.eq_ignore_ascii_case("size") => MinVerify::Size,
        Some(toml::Value::String(s)) if s.eq_ignore_ascii_case("hash") => MinVerify::Hash,
        Some(toml::Value::Table(t)) if t.contains_key("Hash") => MinVerify::Hash,
        _ => MinVerify::None,
    }
}

/// Значение из TOML; `Ok(None)` — пустая тема (не задано).
fn toml_value(key: PolicyKey, v: &toml::Value) -> Result<Option<PolicyValue>, &'static str> {
    match (key.kind(), v) {
        (Kind::Bool, toml::Value::Boolean(b)) => Ok(Some(PolicyValue::Bool(*b))),
        (Kind::Bool, _) => Err("expected true or false"),
        (_, toml::Value::String(s)) => str_value(key, s),
        _ => Err(expected(key)),
    }
}

/// Значение из реестра: булевы — `REG_DWORD`, остальные — `REG_SZ`.
fn gpo_value(key: PolicyKey, v: &GpoValue) -> Result<Option<PolicyValue>, &'static str> {
    match (key.kind(), v) {
        (Kind::Bool, GpoValue::Dword(d)) => Ok(Some(PolicyValue::Bool(*d != 0))),
        (Kind::Bool, _) => Err("expected REG_DWORD"),
        (_, GpoValue::Sz(s)) => str_value(key, s),
        _ => Err("expected REG_SZ"),
    }
}

fn str_value(key: PolicyKey, s: &str) -> Result<Option<PolicyValue>, &'static str> {
    let s = s.trim();
    let value = match (key.kind(), s) {
        (Kind::Theme, "") => return Ok(None),
        (Kind::Theme, t) => PolicyValue::Theme(t.into()),
        (Kind::Verify, "none") => PolicyValue::Verify(MinVerify::None),
        (Kind::Verify, "size") => PolicyValue::Verify(MinVerify::Size),
        (Kind::Verify, "hash") => PolicyValue::Verify(MinVerify::Hash),
        (Kind::SshRecording, "required") => PolicyValue::SshRecording(SshRecording::Required),
        (Kind::SshRecording, "forbidden") => PolicyValue::SshRecording(SshRecording::Forbidden),
        (Kind::SshRecording, "user") => PolicyValue::SshRecording(SshRecording::User),
        _ => return Err(expected(key)),
    };
    Ok(Some(value))
}

fn expected(key: PolicyKey) -> &'static str {
    match key.kind() {
        Kind::Bool => "expected true or false",
        Kind::Theme => "expected a theme name",
        Kind::Verify => "expected none, size or hash",
        Kind::SshRecording => "expected required, forbidden or user",
    }
}
