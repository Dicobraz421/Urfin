//! macOS и Linux: тему ведёт winit, акцент — `#0078D4`, шрифты — встроенные egui.
//! macOS (`controlAccentColor`, системный шрифт через Core Text) — в пассиве,
//! docs/macos-notes.md.

use std::sync::mpsc;

use super::{Context, FontFiles, Look};

pub(super) fn look() -> Look {
    Look::default()
}

pub(super) fn fonts() -> FontFiles {
    FontFiles::default()
}

pub(super) fn watch(_wake: Box<dyn Fn() + Send>, _sink: mpsc::Sender<Look>) {}

pub(super) fn policy_gpo() -> Vec<(urfin_core::PolicyKey, urfin_core::GpoValue)> {
    Vec::new()
}

pub(super) fn watch_policy(
    _dir: Option<std::path::PathBuf>,
    _changed: Box<dyn FnMut() -> bool + Send>,
) {
}

pub(super) fn context() -> Option<Context> {
    None
}
