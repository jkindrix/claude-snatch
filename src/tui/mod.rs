//! Terminal User Interface for claude-snatch.
//!
//! Provides an interactive three-panel interface:
//! - Left: Project/session tree browser
//! - Center: Conversation view
//! - Right: Details panel (metadata, analytics)
//!
//! Built with ratatui for cross-platform terminal support.

mod app;
mod components;
mod events;
mod highlight;
mod state;
mod theme;

pub use app::{run, run_with_theme};
pub use theme::{available_themes, Theme};

use crate::error::Result;

/// Launch the TUI application.
pub fn launch(project: Option<&str>, session: Option<&str>) -> Result<()> {
    app::run(project, session)
}

/// Launch the TUI application with a specific theme.
pub fn launch_with_theme(project: Option<&str>, session: Option<&str>, theme: Option<&str>) -> Result<()> {
    app::run_with_theme(project, session, theme)
}
