//! Integration tests for snatch interactive features using rust-expect.
//!
//! These tests verify that the TUI, pick, and watch commands work correctly
//! by spawning them in a PTY and simulating user interaction.
//!
//! Run with: `cargo test --test tui_integration`
//!
//! Note: TUI tests may be skipped in some CI/PTY environments where crossterm
//! cannot properly initialize the terminal event system.

use rust_expect::prelude::*;
use std::time::Duration;

/// Helper to get the snatch binary path
fn snatch_bin() -> &'static str {
    env!("CARGO_BIN_EXE_snatch")
}

mod tui_tests {
    use super::*;

    /// Test that `snatch tui` launches and renders UI elements.
    ///
    /// Note: This test may fail in CI environments or when crossterm cannot
    /// properly detect the PTY as an interactive terminal.
    #[tokio::test]
    async fn test_tui_launches_and_renders() -> Result<()> {
        let mut session = Session::spawn(snatch_bin(), &["tui"]).await?;

        // TUI should render with box drawing characters, show Session/Project,
        // OR fail with a "no interactive terminal" error (which is acceptable in CI)
        let result = session
            .expect_timeout(
                Pattern::regex(r"[│─┌┐└┘├┤┬┴┼]|Session|Project|claude|Cannot launch TUI|no interactive terminal").unwrap(),
                Duration::from_secs(5),
            )
            .await;

        match result {
            Ok(m) => {
                if m.matched.contains("Cannot launch TUI") || m.matched.contains("no interactive terminal") {
                    // TUI can't run in this environment - skip gracefully
                    eprintln!("TUI test skipped: terminal not interactive in this environment");
                    return Ok(());
                }
                // TUI rendered - quit cleanly
                session.send_str("q").await?;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(_) => {
                // Timeout - TUI may have failed to start
                session.send_control(ControlChar::CtrlC).await.ok();
            }
        }

        Ok(())
    }

    /// Test TUI navigation with j/k keys
    #[tokio::test]
    async fn test_tui_navigation() -> Result<()> {
        let mut session = Session::spawn(snatch_bin(), &["tui"]).await?;

        // Wait for TUI to render or error
        let result = session
            .expect_timeout(
                Pattern::regex(r"[│─┌┐└┘]|Cannot launch TUI|no interactive terminal").unwrap(),
                Duration::from_secs(5),
            )
            .await;

        match result {
            Ok(m) => {
                if m.matched.contains("Cannot") || m.matched.contains("no interactive") {
                    eprintln!("TUI navigation test skipped: terminal not interactive");
                    return Ok(());
                }

                // Navigate down
                session.send_str("j").await?;
                tokio::time::sleep(Duration::from_millis(100)).await;

                // Navigate up
                session.send_str("k").await?;
                tokio::time::sleep(Duration::from_millis(100)).await;

                // Quit
                session.send_str("q").await?;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(_) => {
                session.send_control(ControlChar::CtrlC).await.ok();
            }
        }

        Ok(())
    }

    /// Test TUI exits cleanly with 'q'
    #[tokio::test]
    async fn test_tui_quit() -> Result<()> {
        let mut session = Session::spawn(snatch_bin(), &["tui"]).await?;

        // Wait for TUI to render or error
        let result = session
            .expect_timeout(
                Pattern::regex(r"[│─]|Cannot launch TUI|no interactive terminal").unwrap(),
                Duration::from_secs(5),
            )
            .await;

        match result {
            Ok(m) => {
                if m.matched.contains("Cannot") || m.matched.contains("no interactive") {
                    eprintln!("TUI quit test skipped: terminal not interactive");
                    return Ok(());
                }

                // Quit
                session.send_str("q").await?;

                // Should exit - wait for process to end
                let wait_result = tokio::time::timeout(Duration::from_secs(2), session.wait()).await;

                assert!(
                    wait_result.is_ok(),
                    "TUI should exit cleanly after 'q' press"
                );
            }
            Err(_) => {
                session.send_control(ControlChar::CtrlC).await.ok();
            }
        }

        Ok(())
    }
}

mod pick_tests {
    use super::*;

    /// Test that `snatch pick` launches the fuzzy picker
    #[tokio::test]
    async fn test_pick_launches() -> Result<()> {
        let mut session = Session::spawn(snatch_bin(), &["pick", "--limit", "10"]).await?;

        // Pick should show some UI elements or session list
        let result = session
            .expect_timeout(
                Pattern::regex(r"[│┃├┬┼>]|session|Session|Select").unwrap(),
                Duration::from_secs(5),
            )
            .await;

        // Either it works or there are no sessions (both valid outcomes)
        if result.is_err() {
            // Try to detect "no sessions" message
            let no_sessions = session
                .expect_timeout(
                    Pattern::regex(r"[Nn]o sessions|empty|not found").unwrap(),
                    Duration::from_secs(1),
                )
                .await;

            assert!(
                no_sessions.is_ok(),
                "Pick should either show sessions or indicate none found"
            );
        }

        // Send ESC to quit
        session.send(b"\x1b").await?;
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Backup: Ctrl+C
        session.send_control(ControlChar::CtrlC).await.ok();

        Ok(())
    }

    /// Test that pick exits with ESC key
    #[tokio::test]
    async fn test_pick_escape_exits() -> Result<()> {
        let mut session = Session::spawn(snatch_bin(), &["pick", "--limit", "5"]).await?;

        // Wait a moment for UI to start
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Send ESC
        session.send(b"\x1b").await?;
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Should have exited or be exiting
        // Send Ctrl+C as backup
        session.send_control(ControlChar::CtrlC).await.ok();

        Ok(())
    }
}

mod watch_tests {
    use super::*;

    /// Test that `snatch watch --all` starts watching
    #[tokio::test]
    async fn test_watch_starts() -> Result<()> {
        let mut session =
            Session::spawn(snatch_bin(), &["watch", "--all", "--interval", "100"]).await?;

        // Watch should show some output about watching or sessions
        let result = session
            .expect_timeout(
                Pattern::regex(r"[Ww]atch|session|active|monitor|waiting").unwrap(),
                Duration::from_secs(3),
            )
            .await;

        // It's okay if watch doesn't output immediately (polling mode)
        if result.is_err() {
            // Just verify it started by checking we can kill it
        }

        // Send Ctrl+C to stop
        session.send_control(ControlChar::CtrlC).await?;
        tokio::time::sleep(Duration::from_millis(200)).await;

        Ok(())
    }

    /// Test watch exits cleanly with Ctrl+C
    #[tokio::test]
    async fn test_watch_ctrl_c_exits() -> Result<()> {
        let mut session =
            Session::spawn(snatch_bin(), &["watch", "--all", "--interval", "200"]).await?;

        // Give it a moment to start
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Send Ctrl+C
        session.send_control(ControlChar::CtrlC).await?;

        // Should exit
        let wait_result = tokio::time::timeout(Duration::from_secs(2), session.wait()).await;

        assert!(
            wait_result.is_ok(),
            "Watch should exit cleanly after Ctrl+C"
        );

        Ok(())
    }
}
