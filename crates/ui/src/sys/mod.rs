//! Системный probe (S2 §1.5): тема, акцент, высокая контрастность, файлы
//! шрифтов. Все чтения — до окна, в `app::boot` (поток `font-probe`); в кадре I/O нет.
//! Изменения темы и акцента приходят из [`watch`] (поток `sys-watch`).
//! `unsafe` разрешён только в платформенных модулях (S0 v4 №1).

use std::path::PathBuf;
use std::sync::mpsc;

use urfin_core::ConfigWarning;

use crate::accent::AccentPalette;

/// Время CPU процесса и потока (бенч G-07).
pub mod cpu;
pub mod policy;
#[cfg(windows)]
pub mod prof;

/// Снимок окна для `--screens`.
#[cfg(windows)]
#[path = "capture_win.rs"]
pub mod capture;

/// Записки клавиш keymap с окна GPUI (S3-07).
#[cfg(windows)]
#[path = "key_notes_win.rs"]
pub mod key_notes;

#[cfg(windows)]
#[path = "shell_open_win.rs"]
mod shell_open;

/// Внешняя ссылка документа (`http(s)`, `mailto`): программой Windows на своём коротком
/// потоке (S7 §3.10, решение §13, вопрос 2). UI не ждёт; ошибка — в журнал.
pub fn open_url(url: &str) {
    if !is_external(url) {
        tracing::debug!(url, "md: link scheme is not opened");
        return;
    }
    let url = url.to_owned();
    let run = move || {
        #[cfg(windows)]
        let r = shell_open::shell_execute(&url);
        #[cfg(not(windows))]
        let r: std::io::Result<()> = Err(std::io::Error::from(std::io::ErrorKind::Unsupported));
        if let Err(e) = r {
            tracing::warn!(error = %e, url, "md: open link");
        }
    };
    if let Err(e) = std::thread::Builder::new()
        .name("md-link".into())
        .spawn(run)
    {
        tracing::error!(error = %e, "md: link thread");
    }
}

/// Схемы, которые открывает Ctrl+клик: `http(s)`, `mailto`.
pub fn is_external(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    ["http://", "https://", "mailto:"]
        .iter()
        .any(|p| lower.starts_with(p))
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as os;

#[cfg(windows)]
#[path = "time_win.rs"]
mod time;

#[cfg(not(windows))]
#[path = "time_other.rs"]
mod time;

pub use time::Zone;

/// Местное время для ячеек дат (S2-08).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

#[cfg(not(windows))]
mod other;
#[cfg(not(windows))]
use other as os;

/// То, что меняется на лету: тема, акцент, контрастность.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Look {
    /// `None` — ОС не сказала, тему ведёт winit (`ctx.system_theme()`).
    pub theme: Option<crate::color::Theme>,
    pub accent: AccentPalette,
    pub high_contrast: bool,
    /// Системные цвета при высокой контрастности (Windows, `GetSysColor`).
    pub hc: Option<HcColors>,
}

/// Минимальный набор системных цветов для режима высокой контрастности (S2 §1.5).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct HcColors {
    pub window: crate::color::Color32,
    pub window_text: crate::color::Color32,
    pub highlight: crate::color::Color32,
    pub highlight_text: crate::color::Color32,
    pub gray_text: crate::color::Color32,
    pub hotlight: crate::color::Color32,
}

/// Особый контекст окна (T-01-5): полоса 3 px сверху и метка в заголовке.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Context {
    /// Повышенные права (`TokenElevation`) — `context_admin`.
    Admin,
    /// Удалённый сеанс (`SM_REMOTESESSION`) — `context_remote`.
    Remote,
}

/// Файлы шрифтов ОС по цепочкам S2 §1.5. Пустая цепочка — встроенные шрифты.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FontFiles {
    /// Интерфейс: `SegUIVar.ttf`, иначе `segoeui.ttf` + Semibold.
    pub ui: Vec<PathBuf>,
    /// Иконки: Segoe Fluent Icons, иначе Segoe MDL2 Assets; нет — вшитые (S2-04).
    pub icons: Option<PathBuf>,
    /// Моноширинный: Cascadia Mono, иначе Consolas.
    pub mono: Option<PathBuf>,
    /// Символы вне Segoe UI.
    pub symbols: Option<PathBuf>,
    /// CJK по языку ОС; грузится после первого кадра.
    pub cjk: Option<PathBuf>,
}

/// Итог probe.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SysProbe {
    pub look: Look,
    pub fonts: FontFiles,
    /// Политики организации (ADR-0020): тема, замки настроек.
    pub policy: urfin_core::PolicySet,
    pub context: Option<Context>,
    /// Например, «нет ни одного шрифта интерфейса» — тогда встроенные шрифты egui.
    pub warnings: Vec<ConfigWarning>,
}

/// Probe ОС. Конфиг `[ui]` (свои шрифты, тема) учтут S2-04/S2-05.
pub fn probe_system() -> SysProbe {
    let _s = tracing::info_span!("startup.probe").entered();
    let started = std::time::Instant::now();
    // Linux/macOS: zoneinfo читает и кэширует jiff — здесь, а не в UI-потоке (S2-08).
    #[cfg(not(windows))]
    let _ = Zone::system();
    let mut probe = SysProbe {
        look: os::look(),
        fonts: os::fonts(),
        policy: policy::load(),
        context: os::context(),
        warnings: Vec::new(),
    };
    if cfg!(windows) {
        probe.warnings.extend(font_warning(&probe.fonts));
    }
    tracing::info!(target: "startup.probe", elapsed = ?started.elapsed());
    tracing::debug!(?probe, "system probe");
    probe
}

/// Нет ни одного шрифта интерфейса — встроенные шрифты egui и предупреждение.
fn font_warning(fonts: &FontFiles) -> Option<ConfigWarning> {
    fonts.ui.is_empty().then(|| ConfigWarning {
        key: "ui.fonts".to_owned(),
        msg: urfin_i18n::current().get("ui-warn-no-system-font"),
    })
}

/// Следит за темой и акцентом ОС: новый [`Look`] уходит в `sink`, окно будит `wake`.
/// Windows — `RegNotifyChangeKeyValue` в отдельном потоке (без опроса); остальные ОС —
/// ничего (тему ведёт winit; акцент macOS — при реактивации, docs/macos-notes.md).
pub fn watch(wake: impl Fn() + Send + 'static, sink: mpsc::Sender<Look>) {
    os::watch(Box::new(wake), sink);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_font_chain_warns() {
        let w = font_warning(&FontFiles::default()).expect("warning");
        assert_eq!(w.key, "ui.fonts");
        assert!(!w.msg.starts_with('⟨'), "{}", w.msg);
        let found = FontFiles {
            ui: vec!["segoeui.ttf".into()],
            ..FontFiles::default()
        };
        assert_eq!(font_warning(&found), None);
    }

    #[test]
    fn probe_does_not_panic() {
        let p = probe_system();
        if cfg!(windows) {
            assert!(p.look.theme.is_some());
            assert!(!p.fonts.ui.is_empty(), "{:?}", p.fonts);
        }
    }
}
