//! Urfin — ядро: состояние, шина команд, runtime.
//!
//! Владелец — S1. API — specs/S1.md §2 с изменениями S0-contracts-v2…v5.

#![forbid(unsafe_code)]

pub mod bus;
pub mod cancel;
pub mod cmd;
pub mod config;
pub mod entry;
#[cfg(any(test, feature = "fake"))]
pub mod fake;
mod handlers;
pub mod history;
pub mod ids;
pub mod jobs;
#[cfg(test)]
mod jobs_tests;
pub mod loc;
pub mod mask;
pub mod msg;
mod nav;
#[cfg(test)]
mod nav_tests;
pub mod paths;
pub mod policy;
#[cfg(test)]
mod policy_tests;
pub mod prompt;
pub mod runtime;
pub mod selection;
pub mod services;
pub mod session;
#[cfg(test)]
mod session_tests;
pub mod settings;
#[cfg(test)]
mod settings_tests;
pub mod state;
pub mod tab;
mod text;

pub use bus::{Boot, Closed, CommandSender, Core, Handler, Pump, QuitDecision};
pub use cancel::{CancelToken, Cancelled};
pub use cmd::{
    ArgSpec, Args, CmdError, Command, CommandId, CommandMeta, CommandRegistry, Origin, SideRef,
    Value, ValueKind,
};
pub use config::{CONFIG_VERSION, Config, ConfigWarning, General, LogCfg, PanelsCfg};
pub use entry::{Attrs, Entry, EntryKind};
pub use history::{HistEntry, NavHistory, VisitedDirs};
pub use ids::{Gen, JobId, PromptId, Side, TabId};
pub use jobs::{
    Job, JobHandle, JobRegistry, JobSpec, JobState, JobSummary, Phase, Progress, ProgressSnap,
};
pub use loc::{LocError, Location};
pub use mask::{Mask, MaskError};
pub use msg::{Cx, Msg, MsgInbox, MsgSink, SendError, TrySendError, Waker, msg};
pub use paths::{PORTABLE_MARKER, Paths};
pub use policy::{
    GpoValue, MinVerify, PolicyEntry, PolicyError, PolicyKey, PolicySet, PolicySource, PolicyValue,
    SshRecording,
};
pub use prompt::{Anchor, Choice, Prompt, PromptDetail, PromptSpec, Prompts, Reply};
pub use runtime::{Lane, Runtime, RuntimeConfig, TaskCx};
pub use selection::Selection;
pub use services::{
    ArrangeReq, ArrangeSpec, ListingService, ListingUpdate, LoadReq, NullListing, OpenResolver,
    Patch, Services, TabEvent, listing_msg,
};
pub use session::{
    BookmarkSession, PanelSession, SESSION_VERSION, Session, TabSession, WindowGeom,
};
pub use state::{AppState, Banner, Extensions, Focus, Level, OverlayKind, Panel, UiEvent};
pub use tab::{
    Cursor, Listing, ListingStats, ListingStatus, PanelMode, Row, SortKey, SortSpec, Tab, Targets,
    ViewMode,
};

/// Версия крейта из `Cargo.toml`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_smoke() {
        assert_eq!(super::version(), env!("CARGO_PKG_VERSION"));
        assert_ne!(super::version(), "");
    }
}
