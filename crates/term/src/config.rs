//! Секция `[term]` конфига (S6 §8): `[term]`, `[term.history]`,
//! `[term.complete]`, `[term.highlight]` (F03 §3.3), `[[term.shells]]`.

use compact_str::CompactString;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TermConfig {
    /// `auto` | `cmd` | `pwsh` | `powershell` | `wsl:<D>` | id из `[[term.shells]]`.
    pub default_shell: CompactString,
    /// `auto` | `bundled` | `system` (S0-decisions №28).
    pub conpty: CompactString,
    /// Авто-инъекция shell integration (№29); выключатель на shell — `[[term.shells]]`.
    pub integration: bool,
    /// `both` | `panel_to_shell` | `shell_to_panel` | `off`.
    pub sync_cwd: CompactString,
    /// Через столько мс команда без вывода прячет панели (№27).
    pub auto_show_ms: u32,
    pub return_to_panels: bool,
    pub scrollback: u32,
    pub background_scrollback: u32,
    pub cursor_blink: bool,
    /// `off` | `copy` (OSC 52 только пишет в буфер).
    pub osc52: CompactString,
    pub font_size: f32,
    /// `behind` (за панелями, Ctrl+O) | `split` (зона снизу) — №27.
    pub layout: CompactString,
    pub history: HistoryConfig,
    pub complete: CompleteConfig,
    /// Подсветка вывода (F03 §3.3): общая для всех терминалов.
    pub highlight: crate::hl::HighlightConfig,
    pub shells: Vec<ShellConfig>,
}

impl Default for TermConfig {
    fn default() -> Self {
        TermConfig {
            default_shell: "auto".into(),
            conpty: "auto".into(),
            integration: true,
            sync_cwd: "both".into(),
            auto_show_ms: 300,
            return_to_panels: true,
            scrollback: 10_000,
            background_scrollback: 2_000,
            cursor_blink: false,
            osc52: "copy".into(),
            font_size: 14.0,
            layout: "behind".into(),
            history: HistoryConfig::default(),
            complete: CompleteConfig::default(),
            highlight: crate::hl::HighlightConfig::default(),
            shells: Vec::new(),
        }
    }
}

/// История команд (S6 §6.1, S0-decisions №30).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistoryConfig {
    pub max: u32,
    /// Строки с ведущим пробелом не сохраняются.
    pub ignore_space: bool,
    /// Маски строк, которые не пишутся на диск (секреты).
    pub exclude: Vec<CompactString>,
    pub import_native: bool,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        HistoryConfig {
            max: 5000,
            ignore_space: true,
            exclude: ["*password*", "*passwd*", "*token*", "*secret*"]
                .into_iter()
                .map(CompactString::from)
                .collect(),
            import_native: true,
        }
    }
}

/// Автокомплит (S6 §6.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompleteConfig {
    pub ghost: bool,
    pub pwsh_server: bool,
    pub timeout_ms: u32,
}

impl Default for CompleteConfig {
    fn default() -> Self {
        CompleteConfig {
            ghost: true,
            pwsh_server: true,
            timeout_ms: 300,
        }
    }
}

/// Свой shell (`[[term.shells]]`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellConfig {
    pub id: CompactString,
    pub title: CompactString,
    pub program: CompactString,
    pub args: Vec<CompactString>,
    /// `cmd` | `pwsh` | `powershell` | `bash` | `wsl` | `other`.
    pub kind: CompactString,
    pub integration: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Умолчания `[term]` — снимок (S6-01).
    #[test]
    fn defaults_snapshot() {
        insta::assert_snapshot!(toml::to_string(&TermConfig::default()).unwrap());
    }

    #[test]
    fn partial_section_keeps_defaults() {
        let c: TermConfig =
            toml::from_str("default_shell = \"pwsh\"\n[history]\nmax = 10\n").unwrap();
        assert_eq!(c.default_shell, "pwsh");
        assert_eq!(c.history.max, 10);
        assert!(c.history.ignore_space);
        assert_eq!(c.scrollback, 10_000);
    }
}
