//! TUI launcher command.
//!
//! Launches the interactive TUI interface.

use crate::cli::{Cli, TuiArgs};
use crate::error::{Result, SnatchError};

/// Run the TUI command.
pub fn run(_cli: &Cli, _args: &TuiArgs) -> Result<()> {
    // For now, just indicate that TUI will be launched
    // The actual TUI implementation is in the tui module

    #[cfg(feature = "tui")]
    {
        crate::tui::run(_args.project.as_deref(), _args.session.as_deref())
    }

    #[cfg(not(feature = "tui"))]
    {
        Err(SnatchError::unsupported(
            "TUI feature not enabled. Rebuild with --features tui",
        ))
    }
}
