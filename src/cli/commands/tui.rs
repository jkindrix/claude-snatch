//! TUI launcher command.
//!
//! Launches the interactive TUI interface.

use crate::cli::{Cli, TuiArgs};
use crate::error::{Result, SnatchError};

/// Run the TUI command.
pub fn run(_cli: &Cli, args: &TuiArgs) -> Result<()> {
    // Launch the TUI with optional project, session, theme, and ASCII mode
    #[cfg(feature = "tui")]
    {
        crate::tui::run_with_options(
            args.project.as_deref(),
            args.session.as_deref(),
            args.theme.as_deref(),
            args.ascii,
        )
    }

    #[cfg(not(feature = "tui"))]
    {
        // Suppress unused variable warnings
        let _ = args;
        Err(SnatchError::unsupported(
            "TUI feature not enabled. Rebuild with --features tui",
        ))
    }
}
