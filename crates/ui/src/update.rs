//! Обновление программы (решение 2026-11-11 «self-update»): модель для окна в
//! `AppState.ext` и секция `[update]` конфига. Пишет модель handler `update`
//! приложения — сеть есть только там (крейт `pets-update`); ui рисует баннер «Вышла
//! версия X» и строки раздела «О программе».

use serde::Deserialize;
use urfin_core::{AppState, Config};

/// Ключ конфига выключателя «Проверять обновления» (он же под замком политики
/// `allow_update`).
pub const CHECK_KEY: &str = "update.check";
/// Ключ конфига канала: `stable` или `beta`.
pub const CHANNEL_KEY: &str = "update.channel";

/// `[update]` конфига.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct UpdateCfg {
    /// Проверять раз в сутки в фоне. Выключено — сетевых запросов по расписанию нет,
    /// остаётся «Проверить сейчас».
    pub check: bool,
    /// `stable` или `beta`; не задан — по текущей версии (с дефисом — бета).
    pub channel: Option<String>,
}

impl Default for UpdateCfg {
    /// Включено (решение 2026-11-11 «self-update»), канал — по версии.
    fn default() -> Self {
        UpdateCfg {
            check: true,
            channel: None,
        }
    }
}

impl UpdateCfg {
    /// Секция из конфига; неверная — умолчания (предупреждение пишет загрузка конфига).
    pub fn from_config(c: &Config) -> UpdateCfg {
        c.section::<UpdateCfg>("update").0
    }

    /// Канал «Бета». Не задан или неизвестен — бета у предварительной версии
    /// (`0.2.0-beta.1`), иначе стабильный.
    pub fn beta(&self, version: &str) -> bool {
        match self.channel.as_deref().map(str::trim) {
            Some(c) if c.eq_ignore_ascii_case("beta") => true,
            Some(c) if c.eq_ignore_ascii_case("stable") => false,
            _ => version.contains('-'),
        }
    }
}

/// Что сейчас с обновлением — пояснение в «О программе».
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Idle,
    /// Идёт проверка по кнопке.
    Checking,
    /// Новее текущей версии ничего нет (по кнопке).
    UpToDate,
    /// Вышла версия.
    Available(String),
    /// Скачивается и ставится версия.
    Installing(String),
    /// Проверка по кнопке не удалась: текст ошибки.
    Failed(String),
}

/// Модель обновления в `AppState.ext`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelfUpdate {
    /// Политика организации не запрещает обновление (`allow_update`, `AllowUpdate`):
    /// иначе выключатель заперт, кнопки «Проверить сейчас» нет.
    pub allowed: bool,
    /// В сборке есть открытый ключ подписи: без него проверок нет вовсе.
    pub keyed: bool,
    pub status: Status,
    /// Баннер «Вышла версия X»: версия, пока не нажали «Установить» или «Позже».
    pub offer: Option<String>,
}

impl Default for SelfUpdate {
    fn default() -> Self {
        SelfUpdate {
            allowed: true,
            keyed: false,
            status: Status::Idle,
            offer: None,
        }
    }
}

/// Модель из состояния; нет — умолчания (handler ещё не прошёл первый pump).
pub fn model(s: &AppState) -> SelfUpdate {
    s.ext.get::<SelfUpdate>().cloned().unwrap_or_default()
}

/// Предложенная версия для баннера; не во время установки.
pub fn offer(s: &AppState) -> Option<&str> {
    let m = s.ext.get::<SelfUpdate>()?;
    if matches!(m.status, Status::Installing(_)) {
        return None;
    }
    m.offer.as_deref()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_defaults_to_version_kind() {
        let auto = UpdateCfg::default();
        assert!(!auto.beta("0.1.0"));
        assert!(auto.beta("0.2.0-beta.3"));
        let stable = UpdateCfg {
            channel: Some("stable".into()),
            ..UpdateCfg::default()
        };
        assert!(!stable.beta("0.2.0-beta.3"));
        let beta = UpdateCfg {
            channel: Some(" Beta ".into()),
            ..UpdateCfg::default()
        };
        assert!(beta.beta("0.1.0"));
    }

    #[test]
    fn section_from_config() {
        let mut c = Config::default();
        assert_eq!(UpdateCfg::from_config(&c), UpdateCfg::default());
        c.ext.insert(
            "update".into(),
            toml::Value::Table("check = false\nchannel = \"beta\"".parse().unwrap()),
        );
        let u = UpdateCfg::from_config(&c);
        assert!(!u.check);
        assert_eq!(u.channel.as_deref(), Some("beta"));
    }
}
