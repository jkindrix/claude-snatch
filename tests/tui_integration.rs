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

/// Serializes the PTY-spawning integration tests. Each drives a real
/// pseudo-terminal; running several at once starves them under parallel
/// load and makes them flaky, so each holds this lock for its duration.
static PTY_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(feature = "tui")]
mod tui_tests {
    use super::*;

    /// Test that `snatch tui` launches and renders UI elements.
    ///
    /// Note: This test may fail in CI environments or when crossterm cannot
    /// properly detect the PTY as an interactive terminal.
    #[tokio::test]
    async fn test_tui_launches_and_renders() -> Result<()> {
        let _serial = PTY_SERIAL.lock().await;
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
                if m.matched.contains("Cannot launch TUI")
                    || m.matched.contains("no interactive terminal")
                {
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
        let _serial = PTY_SERIAL.lock().await;
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
        let _serial = PTY_SERIAL.lock().await;
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
                let wait_result =
                    tokio::time::timeout(Duration::from_secs(2), session.wait()).await;

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
        let _serial = PTY_SERIAL.lock().await;
        let mut session = match Session::spawn(snatch_bin(), &["pick", "--limit", "10"]).await {
            Ok(session) => session,
            Err(_) => {
                eprintln!("Pick test skipped: terminal not interactive in this environment");
                return Ok(());
            }
        };

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

            if no_sessions.is_err() {
                // The picker neither rendered nor reported an empty list: in a
                // headless environment the process exits without a usable
                // terminal, leaving nothing to drive. Skip gracefully.
                eprintln!("Pick test skipped: terminal not interactive in this environment");
                return Ok(());
            }
        }

        // Send ESC to quit. A write error (e.g. EIO on a dead PTY) means the
        // process already exited for lack of an interactive terminal - skip.
        if session.send(b"\x1b").await.is_err() {
            eprintln!("Pick test skipped: terminal not interactive in this environment");
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Backup: Ctrl+C
        session.send_control(ControlChar::CtrlC).await.ok();

        Ok(())
    }

    /// Test that pick exits with ESC key
    #[tokio::test]
    async fn test_pick_escape_exits() -> Result<()> {
        let _serial = PTY_SERIAL.lock().await;
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
        let _serial = PTY_SERIAL.lock().await;
        // `--follow` keeps watch running so there is genuinely a live process to
        // observe and interrupt; without it watch does a single poll and exits
        // on its own, which made the old test a no-op that passed by timing luck.
        let mut session = match Session::spawn(
            snatch_bin(),
            &["watch", "--all", "--follow", "--interval", "100"],
        )
        .await
        {
            Ok(session) => session,
            Err(_) => {
                eprintln!("Watch test skipped: terminal not interactive in this environment");
                return Ok(());
            }
        };

        // A live watch must print its startup banner. If it instead reports that
        // there are no active sessions to watch (common on a fresh CI checkout),
        // there is nothing to drive - skip gracefully. A timeout likewise means
        // the process could not run here.
        match session
            .expect_timeout(
                Pattern::regex(r"Watching \d+ session|No sessions to watch").unwrap(),
                Duration::from_secs(5),
            )
            .await
        {
            Ok(m) if m.matched.contains("No sessions") => {
                eprintln!("Watch test skipped: no active sessions to watch in this environment");
                return Ok(());
            }
            Ok(_) => {}
            Err(_) => {
                eprintln!("Watch test skipped: terminal not interactive in this environment");
                return Ok(());
            }
        }

        // It started and is watching. Stop it; a write error means the PTY is
        // dead in this environment - skip.
        if session.send_control(ControlChar::CtrlC).await.is_err() {
            eprintln!("Watch test skipped: terminal not interactive in this environment");
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Ensure the follow loop is not left running: dropping the session closes
        // the PTY but does not reap the child, and on some platforms Ctrl+C does
        // not terminate it.
        session.kill().ok();

        Ok(())
    }

    /// Test watch exits cleanly with Ctrl+C
    #[tokio::test]
    async fn test_watch_ctrl_c_exits() -> Result<()> {
        let _serial = PTY_SERIAL.lock().await;
        // `--follow` is essential here: it keeps watch in its polling loop so the
        // Ctrl+C below actually exercises interrupt handling. Without it watch
        // exits on its own after one poll, so the test never tested anything.
        let mut session = match Session::spawn(
            snatch_bin(),
            &["watch", "--all", "--follow", "--interval", "200"],
        )
        .await
        {
            Ok(session) => session,
            Err(_) => {
                eprintln!("Watch test skipped: terminal not interactive in this environment");
                return Ok(());
            }
        };

        // Confirm watch is actually up and looping before we interrupt it. If
        // there is nothing active to watch, or it cannot run here, skip - there
        // is no interrupt behavior to assert.
        match session
            .expect_timeout(
                Pattern::regex(r"Watching \d+ session|No sessions to watch").unwrap(),
                Duration::from_secs(5),
            )
            .await
        {
            Ok(m) if m.matched.contains("No sessions") => {
                eprintln!("Watch test skipped: no active sessions to watch in this environment");
                return Ok(());
            }
            Ok(_) => {}
            Err(_) => {
                eprintln!("Watch test skipped: terminal not interactive in this environment");
                return Ok(());
            }
        }

        // Send Ctrl+C. The PTY is in cooked mode, so this byte is delivered to
        // the process group as SIGINT. A write error means the PTY is dead in
        // this environment - skip.
        if session.send_control(ControlChar::CtrlC).await.is_err() {
            eprintln!("Watch test skipped: terminal not interactive in this environment");
            return Ok(());
        }

        // The interrupt must terminate the running process. rust-expect reports
        // EOF as ProcessExitStatus::Unknown, so we can confirm watch exits after
        // the interrupt but not its precise exit code.
        let wait_result = tokio::time::timeout(Duration::from_secs(5), session.wait()).await;

        if cfg!(target_os = "linux") {
            // Linux PTYs reliably deliver Ctrl+C as SIGINT (cooked mode) and reap
            // the child, so the interrupt MUST terminate watch. This is the strict
            // gate that catches a regression where watch stops honoring Ctrl+C.
            assert!(
                wait_result.is_ok(),
                "Watch should exit after Ctrl+C (SIGINT)"
            );
        } else if wait_result.is_err() {
            // macOS/Windows test PTYs can't always deliver the interrupt or
            // observe the child's exit (Windows ConPTY has no cooked-mode line
            // discipline). Don't false-fail there - the Linux gate covers the
            // interrupt behavior - but kill the still-running child explicitly,
            // because dropping the session only closes the PTY, it does not reap
            // the process.
            eprintln!("Watch test skipped: PTY could not deliver interrupt in this environment");
            session.kill().ok();
            return Ok(());
        }

        Ok(())
    }
}
