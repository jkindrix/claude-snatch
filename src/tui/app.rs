//! TUI application main loop.

use std::io;

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, KeyCode, KeyModifiers, MouseButton, MouseEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame, Terminal,
};

/// Style for selected text (inverted colors).
fn selection_style() -> Style {
    Style::default()
        .bg(Color::White)
        .fg(Color::Black)
}

use crate::error::{Result, SnatchError};

use super::components::{ScrollableText, StatusBar};
use super::events::{Event, EventHandler, KeyBindings};
use super::state::AppState;
use super::theme::available_themes;

/// Total number of lines in the help overlay (used for scroll bounds).
const HELP_LINE_COUNT: usize = 53;

/// Run the TUI application.
pub fn run(project: Option<&str>, session: Option<&str>) -> Result<()> {
    run_with_options(project, session, None, false)
}

/// Run the TUI application with a specific theme.
pub fn run_with_theme(project: Option<&str>, session: Option<&str>, theme: Option<&str>) -> Result<()> {
    run_with_options(project, session, theme, false)
}

/// Run the TUI application with all options.
pub fn run_with_options(
    project: Option<&str>,
    session: Option<&str>,
    theme: Option<&str>,
    ascii_mode: bool,
) -> Result<()> {
    // Setup terminal
    enable_raw_mode().map_err(|e| {
        SnatchError::io(
            "Cannot launch TUI - no interactive terminal available. \
             The TUI requires a terminal with keyboard input support",
            e,
        )
    })?;

    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .map_err(|e| SnatchError::io("Failed to enter alternate screen", e))?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)
        .map_err(|e| SnatchError::io("Failed to create terminal", e))?;

    // Create app state with optional theme
    let mut app = AppState::with_theme(theme)?;
    app.ascii_mode = ascii_mode;

    // Load initial data
    if let Some(session_id) = session {
        app.select_session(session_id)?;
    } else if let Some(project_path) = project {
        app.select_project(project_path)?;
    }

    // Main loop
    let result = run_loop(&mut terminal, &mut app);

    // Restore terminal
    disable_raw_mode().map_err(|e| SnatchError::io("Failed to disable raw mode", e))?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .map_err(|e| SnatchError::io("Failed to leave alternate screen", e))?;
    terminal.show_cursor().map_err(|e| SnatchError::io("Failed to show cursor", e))?;

    result
}

/// Main event loop using EventHandler.
fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut AppState,
) -> Result<()> {
    // Create event handler with 100ms tick rate
    let events = EventHandler::new(std::time::Duration::from_millis(100));
    let bindings = KeyBindings::default();

    loop {
        // Draw UI
        terminal.draw(|f| draw_ui(f, app))
            .map_err(|e| SnatchError::io("Failed to draw TUI", e))?;

        // Handle events from the event handler
        match events.next() {
            Ok(Event::Key(key)) => {
                // Clear status message on any key press
                app.status_message = None;

                // Handle search mode input first
                if app.is_searching() {
                    match (key.modifiers, key.code) {
                        // Exit search mode
                        (KeyModifiers::NONE, KeyCode::Esc) => {
                            app.cancel_search();
                            continue;
                        }
                        // Confirm search (exit but keep results)
                        (KeyModifiers::NONE, KeyCode::Enter) => {
                            app.search_state.active = false;
                            continue;
                        }
                        // Navigate results
                        (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::CONTROL, KeyCode::Char('n')) => {
                            app.search_next();
                            continue;
                        }
                        (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::CONTROL, KeyCode::Char('p')) => {
                            app.search_prev();
                            continue;
                        }
                        // Backspace
                        (KeyModifiers::NONE, KeyCode::Backspace) => {
                            app.search_backspace();
                            continue;
                        }
                        // Character input
                        (KeyModifiers::NONE, KeyCode::Char(c)) | (KeyModifiers::SHIFT, KeyCode::Char(c)) => {
                            app.search_input(c);
                            continue;
                        }
                        _ => continue,
                    }
                }

                // Handle command palette input
                if app.is_command_palette_active() {
                    match (key.modifiers, key.code) {
                        // Close palette
                        (KeyModifiers::NONE, KeyCode::Esc) => {
                            app.close_command_palette();
                            continue;
                        }
                        // Execute selected command
                        (KeyModifiers::NONE, KeyCode::Enter) => {
                            if let Err(e) = app.execute_selected_command() {
                                app.status_message = Some(format!("Error: {e}"));
                            }
                            continue;
                        }
                        // Navigate up
                        (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::CONTROL, KeyCode::Char('p')) => {
                            app.command_palette.select_prev();
                            continue;
                        }
                        // Navigate down
                        (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::CONTROL, KeyCode::Char('n')) => {
                            app.command_palette.select_next();
                            continue;
                        }
                        // Backspace
                        (KeyModifiers::NONE, KeyCode::Backspace) => {
                            app.command_palette.backspace();
                            continue;
                        }
                        // Character input
                        (KeyModifiers::NONE, KeyCode::Char(c)) | (KeyModifiers::SHIFT, KeyCode::Char(c)) => {
                            app.command_palette.push_char(c);
                            continue;
                        }
                        _ => continue,
                    }
                }

                // Handle export dialog input
                if app.is_exporting() {
                    match (key.modifiers, key.code) {
                        // Cancel export
                        (KeyModifiers::NONE, KeyCode::Esc) | (_, KeyCode::Char('q')) => {
                            app.cancel_export();
                            continue;
                        }
                        // Confirm export
                        (KeyModifiers::NONE, KeyCode::Enter) => {
                            if let Err(e) = app.confirm_export() {
                                app.export_dialog.status_message = Some(format!("Error: {e}"));
                            }
                            continue;
                        }
                        // Navigate format (left/right or h/l)
                        (KeyModifiers::NONE, KeyCode::Left) | (KeyModifiers::NONE, KeyCode::Char('h')) => {
                            app.export_dialog.prev_format();
                            continue;
                        }
                        (KeyModifiers::NONE, KeyCode::Right) | (KeyModifiers::NONE, KeyCode::Char('l')) => {
                            app.export_dialog.next_format();
                            continue;
                        }
                        // Toggle thinking (t)
                        (KeyModifiers::NONE, KeyCode::Char('t')) => {
                            app.export_dialog.include_thinking = !app.export_dialog.include_thinking;
                            continue;
                        }
                        // Toggle tools (o)
                        (KeyModifiers::NONE, KeyCode::Char('o')) => {
                            app.export_dialog.include_tools = !app.export_dialog.include_tools;
                            continue;
                        }
                        _ => continue,
                    }
                }

                // Handle filter input mode (dates, model, etc.)
                if app.is_entering_input() {
                    match (key.modifiers, key.code) {
                        // Cancel input
                        (KeyModifiers::NONE, KeyCode::Esc) => {
                            app.cancel_filter_input();
                            continue;
                        }
                        // Confirm input
                        (KeyModifiers::NONE, KeyCode::Enter) => {
                            app.confirm_filter_input();
                            continue;
                        }
                        // Backspace
                        (KeyModifiers::NONE, KeyCode::Backspace) => {
                            app.filter_backspace();
                            continue;
                        }
                        // Character input
                        (KeyModifiers::NONE, KeyCode::Char(c)) => {
                            app.filter_input(c);
                            continue;
                        }
                        _ => continue,
                    }
                }

                // Handle help overlay input
                if app.show_help {
                    match (key.modifiers, key.code) {
                        // Close help
                        (KeyModifiers::NONE, KeyCode::Esc)
                        | (KeyModifiers::NONE, KeyCode::Char('?'))
                        | (KeyModifiers::NONE, KeyCode::Char('q')) => {
                            app.toggle_help();
                            continue;
                        }
                        // Scroll up
                        (KeyModifiers::NONE, KeyCode::Up)
                        | (KeyModifiers::NONE, KeyCode::Char('k')) => {
                            app.help_scroll_up();
                            continue;
                        }
                        // Scroll down
                        (KeyModifiers::NONE, KeyCode::Down)
                        | (KeyModifiers::NONE, KeyCode::Char('j')) => {
                            // HELP_LINE_COUNT is the total number of lines in help text
                            // We pass a reasonable visible height estimate
                            app.help_scroll_down(HELP_LINE_COUNT, 30);
                            continue;
                        }
                        // Page up
                        (KeyModifiers::NONE, KeyCode::PageUp) => {
                            for _ in 0..10 {
                                app.help_scroll_up();
                            }
                            continue;
                        }
                        // Page down
                        (KeyModifiers::NONE, KeyCode::PageDown) => {
                            for _ in 0..10 {
                                app.help_scroll_down(HELP_LINE_COUNT, 30);
                            }
                            continue;
                        }
                        // Scroll to top
                        (KeyModifiers::NONE, KeyCode::Home) => {
                            app.help_scroll = 0;
                            continue;
                        }
                        // Scroll to bottom
                        (KeyModifiers::NONE, KeyCode::End) => {
                            app.help_scroll = HELP_LINE_COUNT.saturating_sub(30);
                            continue;
                        }
                        _ => continue,
                    }
                }

                // Check configurable key bindings first
                if bindings.is_quit(&key) {
                    return Ok(());
                }

                if bindings.is_up(&key) {
                    app.previous();
                    continue;
                }

                if bindings.is_down(&key) {
                    // Calculate visible height for tree panel scrolling
                    // Tree panel height = terminal height - status bar (3 lines) - borders (2)
                    let tree_height = terminal.size()
                        .map(|s| s.height.saturating_sub(5) as usize)
                        .unwrap_or(20);
                    app.next(tree_height);
                    continue;
                }

                if bindings.is_left(&key) {
                    app.focus_left();
                    continue;
                }

                if bindings.is_right(&key) {
                    app.focus_right();
                    continue;
                }

                if bindings.is_select(&key) {
                    app.select()?;
                    continue;
                }

                if bindings.is_back(&key) {
                    app.back();
                    continue;
                }

                // Handle other keys by code and modifiers
                match (key.modifiers, key.code) {

                    // Panel toggles
                    (KeyModifiers::NONE, KeyCode::Char('1')) => {
                        app.set_focus(0);
                    }
                    (KeyModifiers::NONE, KeyCode::Char('2')) => {
                        app.set_focus(1);
                    }
                    (KeyModifiers::NONE, KeyCode::Char('3')) => {
                        app.set_focus(2);
                    }

                    // Scroll
                    (KeyModifiers::NONE, KeyCode::PageUp) => {
                        app.scroll_up(10);
                    }
                    (KeyModifiers::NONE, KeyCode::PageDown) => {
                        app.scroll_down(10);
                    }
                    (KeyModifiers::NONE, KeyCode::Home) => {
                        app.scroll_to_top();
                    }
                    (KeyModifiers::NONE, KeyCode::End) => {
                        app.scroll_to_bottom();
                    }

                    // Search
                    (KeyModifiers::NONE, KeyCode::Char('/')) => {
                        app.start_search();
                    }
                    // Next search result (n)
                    (KeyModifiers::NONE, KeyCode::Char('n')) => {
                        if !app.search_state.results.is_empty() {
                            app.search_next();
                        }
                    }
                    // Previous search result (N)
                    (KeyModifiers::SHIFT, KeyCode::Char('N')) => {
                        if !app.search_state.results.is_empty() {
                            app.search_prev();
                        }
                    }

                    // Refresh
                    (KeyModifiers::NONE, KeyCode::Char('r')) => {
                        app.refresh()?;
                    }

                    // Export
                    (KeyModifiers::NONE, KeyCode::Char('e')) => {
                        app.export()?;
                    }

                    // Copy message to clipboard
                    (KeyModifiers::NONE, KeyCode::Char('c')) => {
                        app.copy_message()?;
                    }

                    // Copy code block to clipboard
                    (KeyModifiers::SHIFT, KeyCode::Char('C')) => {
                        app.copy_code_block()?;
                    }

                    // Open in external editor
                    (KeyModifiers::SHIFT, KeyCode::Char('O')) => {
                        app.open_in_editor()?;
                    }

                    // Resume session in Claude Code
                    (KeyModifiers::SHIFT, KeyCode::Char('R')) => {
                        app.resume_in_claude()?;
                    }

                    // Toggle thinking
                    (KeyModifiers::NONE, KeyCode::Char('t')) => {
                        app.toggle_thinking();
                    }

                    // Toggle tools
                    (KeyModifiers::NONE, KeyCode::Char('o')) => {
                        app.toggle_tools();
                    }

                    // Toggle word wrap
                    (KeyModifiers::NONE, KeyCode::Char('w')) => {
                        app.toggle_word_wrap();
                    }

                    // Toggle line numbers
                    (KeyModifiers::NONE, KeyCode::Char('#')) => {
                        app.toggle_line_numbers();
                    }

                    // Focus mode (zen mode - hide side panels)
                    (KeyModifiers::NONE, KeyCode::Char('z')) => {
                        app.toggle_focus_mode();
                    }

                    // Filter controls
                    (KeyModifiers::NONE, KeyCode::Char('f')) => {
                        app.toggle_filter();
                    }
                    (KeyModifiers::SHIFT, KeyCode::Char('F')) => {
                        app.cycle_message_filter();
                    }
                    (KeyModifiers::SHIFT, KeyCode::Char('B')) => {
                        app.reverse_cycle_message_filter();
                    }
                    (KeyModifiers::SHIFT, KeyCode::Char('E')) => {
                        app.toggle_errors_filter();
                    }
                    (KeyModifiers::SHIFT, KeyCode::Char('X')) => {
                        app.clear_filters();
                    }
                    // Date range filters
                    (KeyModifiers::NONE, KeyCode::Char('[')) => {
                        app.start_date_from_input();
                    }
                    (KeyModifiers::NONE, KeyCode::Char(']')) => {
                        app.start_date_to_input();
                    }
                    // Model filter
                    (KeyModifiers::SHIFT, KeyCode::Char('M')) => {
                        app.toggle_model_filter();
                    }

                    // Go to line number (Ctrl+G or G)
                    (KeyModifiers::CONTROL, KeyCode::Char('g')) | (KeyModifiers::SHIFT, KeyCode::Char('G')) => {
                        app.start_goto_line();
                    }

                    // Command palette (Ctrl+P)
                    (KeyModifiers::CONTROL, KeyCode::Char('p')) => {
                        app.open_command_palette();
                    }

                    // Cycle theme
                    (KeyModifiers::NONE, KeyCode::Char('T')) => {
                        app.cycle_theme();
                    }

                    // Help
                    (KeyModifiers::NONE, KeyCode::Char('?')) => {
                        app.toggle_help();
                    }

                    // Toggle session selection (space)
                    (KeyModifiers::NONE, KeyCode::Char(' ')) => {
                        if app.focus == 0 && app.current_project.is_some() {
                            // In session list, toggle selection
                            if app.toggle_session_selection() {
                                let count = app.selected_session_count();
                                if count > 0 {
                                    app.status_message = Some(format!("{} session{} selected", count, if count == 1 { "" } else { "s" }));
                                } else {
                                    app.status_message = None;
                                }
                            }
                        }
                    }

                    // Yank (copy) text selection to clipboard (y - vim convention)
                    (KeyModifiers::NONE, KeyCode::Char('y')) => {
                        if app.has_selection() {
                            let _ = app.copy_selection();
                        }
                    }

                    // Select all sessions (Ctrl+A)
                    (KeyModifiers::CONTROL, KeyCode::Char('a')) => {
                        if app.focus == 0 && app.current_project.is_some() {
                            app.select_all_sessions();
                            let count = app.selected_session_count();
                            app.status_message = Some(format!("{} session{} selected", count, if count == 1 { "" } else { "s" }));
                        }
                    }

                    // Clear text selection (Escape when not in modal)
                    (KeyModifiers::NONE, KeyCode::Esc) => {
                        if app.has_selection() {
                            app.clear_selection();
                            app.status_message = Some("Selection cleared".to_string());
                        } else if app.selected_session_count() > 0 {
                            app.clear_session_selection();
                            app.status_message = Some("Session selection cleared".to_string());
                        }
                    }

                    _ => {}
                }
            }
            Ok(Event::Tick) => {
                // Tick event - could be used for animations or updates
            }
            Ok(Event::Resize(_, _)) => {
                // Terminal resize is handled automatically by ratatui
            }
            Ok(Event::Mouse(mouse)) => {
                // Handle mouse events
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        app.scroll_up(3);
                    }
                    MouseEventKind::ScrollDown => {
                        app.scroll_down(3);
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        // Check for double-click (within 500ms at same position)
                        let now = std::time::Instant::now();
                        let is_double_click = app.last_click_time
                            .map(|t| now.duration_since(t).as_millis() < 500)
                            .unwrap_or(false)
                            && app.last_click_pos == (mouse.column, mouse.row);

                        // Update last click tracking
                        app.last_click_time = Some(now);
                        app.last_click_pos = (mouse.column, mouse.row);

                        // Determine which panel was clicked based on x position
                        // Layout is 25% / 50% / 25% (tree / conversation / details)
                        let size = terminal.size().unwrap_or_default();
                        let terminal_width = size.width;
                        let terminal_height = size.height;
                        let tree_width = terminal_width / 4;  // 25%
                        let conversation_width = terminal_width / 2;  // 50%
                        let conversation_end = tree_width + conversation_width;  // 75%

                        // Status bar takes 3 lines at bottom
                        let main_height = terminal_height.saturating_sub(3);

                        // Clear any existing selection on new click
                        app.clear_selection();

                        if mouse.column < tree_width {
                            app.set_focus(0); // Tree panel (left 25%)
                            // Start text selection
                            let bounds = (0, 0, tree_width, main_height);
                            app.start_selection(mouse.column, mouse.row, 0, bounds);

                            // Also select the clicked item if within bounds
                            if mouse.row >= 1 && mouse.row < main_height {
                                let clicked_item = (mouse.row as usize - 1) + app.tree_scroll;
                                if clicked_item < app.tree_items.len() {
                                    app.tree_selected = Some(clicked_item);
                                    // Double-click acts like Enter (select/expand)
                                    if is_double_click {
                                        app.clear_selection(); // Don't start selection on double-click
                                        let _ = app.select();
                                    }
                                }
                            }
                        } else if mouse.column < conversation_end {
                            app.set_focus(1); // Conversation panel (middle 50%)
                            // Start text selection
                            let bounds = (tree_width, 0, conversation_width, main_height);
                            app.start_selection(mouse.column, mouse.row, 1, bounds);
                        } else {
                            app.set_focus(2); // Details panel (right 25%)
                            // Start text selection
                            let details_width = terminal_width.saturating_sub(conversation_end);
                            let bounds = (conversation_end, 0, details_width, main_height);
                            app.start_selection(mouse.column, mouse.row, 2, bounds);
                        }
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        // Update selection during drag
                        app.update_selection(mouse.column, mouse.row);
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        // End selection (but keep it for copying)
                        app.end_selection();
                    }
                    _ => {}
                }
            }
            Err(_) => {
                // Channel closed, exit
                return Ok(());
            }
        }
    }
}

/// Draw the UI.
fn draw_ui(f: &mut Frame, app: &AppState) {
    // Main layout: content area + search bar (if active) + status bar
    let search_height = if app.is_searching() { 3 } else { 0 };
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(search_height),
            Constraint::Length(1),
        ])
        .split(f.area());

    // Focus mode: show only the conversation panel at full width
    if app.focus_mode {
        draw_conversation_panel(f, app, main_chunks[0]);
        render_selection_overlay(f, app, main_chunks[0], 1);
    } else {
        // Normal mode: three columns
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(25),
                Constraint::Percentage(50),
                Constraint::Percentage(25),
            ])
            .split(main_chunks[0]);

        // Left panel: Project/Session tree
        draw_tree_panel(f, app, chunks[0]);
        render_selection_overlay(f, app, chunks[0], 0);

        // Center panel: Conversation
        draw_conversation_panel(f, app, chunks[1]);
        render_selection_overlay(f, app, chunks[1], 1);

        // Right panel: Details
        draw_details_panel(f, app, chunks[2]);
        render_selection_overlay(f, app, chunks[2], 2);
    }

    // Search bar (if active)
    if app.is_searching() {
        draw_search_bar(f, app, main_chunks[1]);
    }

    // Status bar at bottom
    draw_status_bar(f, app, main_chunks[2]);

    // Help overlay if active
    if app.show_help {
        draw_help_overlay(f, app);
    }

    // Export dialog overlay if active
    if app.is_exporting() {
        draw_export_dialog(f, app);
    }

    // Command palette overlay if active
    if app.is_command_palette_active() {
        draw_command_palette(f, app);
    }
}

/// Draw the search bar.
fn draw_search_bar(f: &mut Frame, app: &AppState, area: Rect) {
    let search_text = format!(
        "/{}{} [{}]",
        &app.search_state.query,
        if app.is_searching() { "█" } else { "" },
        app.search_state.result_count_str()
    );

    let style = Style::default()
        .fg(app.theme.primary)
        .add_modifier(Modifier::BOLD);

    let paragraph = Paragraph::new(search_text)
        .style(style)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(app.theme.warning))
                .title(" Search (Enter to confirm, Esc to cancel) "),
        );

    f.render_widget(paragraph, area);
}

/// Draw the tree panel (projects and sessions).
fn draw_tree_panel(f: &mut Frame, app: &AppState, area: Rect) {
    let border_style = if app.focus == 0 {
        app.theme.border_focused_style()
    } else {
        app.theme.border_style()
    };

    // Calculate visible area (subtract 2 for borders)
    let visible_height = area.height.saturating_sub(2) as usize;

    // Build only visible items using tree_scroll offset
    let items: Vec<ListItem> = app
        .tree_items
        .iter()
        .enumerate()
        .skip(app.tree_scroll)
        .take(visible_height)
        .map(|(i, item)| {
            let is_cursor = Some(i) == app.tree_selected;
            let is_selected = app.is_tree_item_selected(i);

            // Determine display text with selection indicator
            let display_text = if is_selected {
                format!("● {}", item) // Selected indicator
            } else {
                format!("  {}", item) // Padding for alignment
            };

            let style = if is_cursor {
                app.theme.selection_style()
            } else if is_selected {
                // Selected but not cursor - show with different style
                Style::default().fg(app.theme.success)
            } else {
                Style::default()
            };
            ListItem::new(display_text).style(style)
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Projects ")
                .borders(Borders::ALL)
                .border_style(border_style),
        );

    f.render_widget(list, area);
}

/// Draw the conversation panel.
fn draw_conversation_panel(f: &mut Frame, app: &AppState, area: Rect) {
    let border_style = if app.focus == 1 {
        app.theme.border_focused_style()
    } else {
        app.theme.border_style()
    };

    let title = if let Some(session_id) = &app.current_session {
        format!(" Session: {} ", &session_id[..8.min(session_id.len())])
    } else {
        " Conversation ".to_string()
    };

    let content = if app.conversation_lines.is_empty() {
        vec![Line::from("Select a session to view")]
    } else if app.show_line_numbers {
        // Add line numbers to each line
        let total_lines = app.conversation_lines.len();
        let line_number_width = total_lines.to_string().len();

        app.conversation_lines
            .iter()
            .enumerate()
            .skip(app.scroll_offset)
            .take(area.height as usize - 2)
            .map(|(i, line)| {
                // Create line number span with dimmed style
                let line_num = format!("{:>width$}│ ", i + 1, width = line_number_width);
                let mut spans = vec![Span::styled(
                    line_num,
                    Style::default().fg(app.theme.secondary).add_modifier(Modifier::DIM),
                )];
                // Append original line content
                spans.extend(line.spans.iter().cloned());
                Line::from(spans)
            })
            .collect()
    } else {
        app.conversation_lines
            .iter()
            .skip(app.scroll_offset)
            .take(area.height as usize - 2)
            .cloned()
            .collect()
    };

    let mut paragraph = Paragraph::new(content)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(border_style),
        );

    // Apply word wrap if enabled
    if app.word_wrap {
        paragraph = paragraph.wrap(Wrap { trim: false });
    }

    f.render_widget(paragraph, area);
}

/// Draw the details panel using ScrollableText component.
fn draw_details_panel(f: &mut Frame, app: &AppState, area: Rect) {
    let content = if app.details_lines.is_empty() {
        vec![Line::from("No details available")]
    } else {
        app.details_lines.clone()
    };

    ScrollableText::new(" Details ")
        .content(content)
        .scroll(app.details_scroll)
        .focused(app.focus == 2)
        .render(f, area);
}

/// Draw the status bar.
fn draw_status_bar(f: &mut Frame, app: &AppState, area: Rect) {
    let mode = if app.is_exporting() {
        "EXPORT"
    } else if app.is_searching() {
        "SEARCH"
    } else if app.is_entering_date() {
        "DATE INPUT"
    } else if app.show_help {
        "HELP"
    } else {
        match app.focus {
            0 => "TREE",
            1 => "CONVERSATION",
            2 => "DETAILS",
            _ => "UNKNOWN",
        }
    };

    let left_content = if app.is_entering_input() {
        // Show input prompt with current buffer
        use super::state::InputMode;
        let (label, hint) = match app.input_mode() {
            InputMode::DateFrom => ("From", "YYYY-MM-DD"),
            InputMode::DateTo => ("To", "YYYY-MM-DD"),
            InputMode::Model => ("Model", "e.g., sonnet, opus"),
            InputMode::LineNumber => ("Go to line", "line number"),
            InputMode::None => ("Input", ""),
        };
        vec![
            Span::styled(" snatch ", Style::default().fg(app.theme.primary).add_modifier(Modifier::BOLD)),
            Span::raw("│ "),
            Span::styled(format!("{}: ", label), Style::default().fg(app.theme.warning)),
            Span::styled(app.input_buffer().to_string(), Style::default().fg(app.theme.primary).add_modifier(Modifier::BOLD)),
            Span::styled("█", Style::default().fg(app.theme.primary)),
            Span::raw(format!(" ({}, Enter to confirm, Esc to cancel)", hint)),
        ]
    } else if let Some(ref msg) = app.status_message {
        // Show status message if present
        vec![
            Span::styled(" snatch ", Style::default().fg(app.theme.primary).add_modifier(Modifier::BOLD)),
            Span::raw("│ "),
            Span::styled(msg.as_str(), Style::default().fg(app.theme.success)),
        ]
    } else {
        vec![
            Span::styled(" snatch ", Style::default().fg(app.theme.primary).add_modifier(Modifier::BOLD)),
            Span::raw("│ "),
            Span::styled(mode, Style::default().fg(app.theme.warning)),
            Span::raw(" │ "),
            Span::styled(&app.theme.name, Style::default().fg(app.theme.secondary)),
        ]
    };

    let right_content = if let Some(session_id) = &app.current_session {
        let short_id = &session_id[..8.min(session_id.len())];
        let total_lines = app.conversation_lines.len();
        let current_line = app.scroll_offset + 1;

        let mut content = vec![];

        // Show focus mode indicator
        if app.focus_mode {
            content.push(Span::styled(
                "[ZEN] ",
                Style::default().fg(app.theme.success).add_modifier(Modifier::BOLD),
            ));
        }

        // Show line position (current/total)
        content.push(Span::raw(format!(
            "L{}/{} ",
            current_line.min(total_lines),
            total_lines
        )));

        // Show entry count
        if app.total_entries > 0 {
            content.push(Span::raw(format!("({} entries) ", app.total_entries)));
        }

        // Show active filter indicator
        if app.filter_state.is_filtering() {
            content.push(Span::raw("│ "));
            content.push(Span::styled(
                format!("[{}]", app.filter_state.summary()),
                Style::default().fg(app.theme.warning),
            ));
        }

        content.push(Span::raw(" │ "));
        content.push(Span::raw(format!("Session: {short_id} ")));
        content
    } else {
        vec![
            Span::raw(format!("{} projects ", app.projects.len())),
            Span::raw("│ "),
            Span::raw("? for help "),
        ]
    };

    StatusBar::new()
        .left(left_content)
        .right(right_content)
        .render(f, area);
}

/// Draw help overlay with scroll support.
fn draw_help_overlay(f: &mut Frame, app: &AppState) {
    let area = centered_rect(60, 70, f.area());
    // Calculate visible height (area height minus borders)
    let visible_height = area.height.saturating_sub(2) as usize;

    let help_text = vec![
        Line::from(Span::styled("Keyboard Shortcuts", Style::default().add_modifier(Modifier::BOLD))),
        Line::from(""),
        Line::from("Navigation:"),
        Line::from("  j/↓       Move down"),
        Line::from("  k/↑       Move up"),
        Line::from("  h/←       Focus left panel"),
        Line::from("  l/→       Focus right panel"),
        Line::from("  Enter     Select/expand"),
        Line::from("  Esc       Go back/close help"),
        Line::from(""),
        Line::from("Panels:"),
        Line::from("  1         Focus tree panel"),
        Line::from("  2         Focus conversation panel"),
        Line::from("  3         Focus details panel"),
        Line::from(""),
        Line::from("Search:"),
        Line::from("  /         Start search"),
        Line::from("  n         Next search result"),
        Line::from("  N         Previous search result"),
        Line::from("  Enter     Confirm search"),
        Line::from("  Esc       Cancel search"),
        Line::from(""),
        Line::from("Actions:"),
        Line::from("  r         Refresh"),
        Line::from("  e         Export session"),
        Line::from("  c         Copy message to clipboard"),
        Line::from("  C         Copy code block to clipboard"),
        Line::from("  y         Yank (copy) text selection"),
        Line::from("  t         Toggle thinking blocks"),
        Line::from("  o         Toggle tool outputs"),
        Line::from("  w         Toggle word wrap"),
        Line::from("  #         Toggle line numbers"),
        Line::from("  z         Toggle focus mode (hide panels)"),
        Line::from(format!("  T         Cycle theme ({})", available_themes().join("/"))),
        Line::from(""),
        Line::from("Filters:"),
        Line::from("  f         Toggle filter panel"),
        Line::from("  F         Cycle message type filter (next)"),
        Line::from("  B         Cycle message type filter (prev)"),
        Line::from("  E         Toggle errors-only filter"),
        Line::from("  M         Filter by model (e.g., sonnet, opus)"),
        Line::from("  [         Set date-from filter (YYYY-MM-DD)"),
        Line::from("  ]         Set date-to filter (YYYY-MM-DD)"),
        Line::from("  X         Clear all filters"),
        Line::from(""),
        Line::from("Selection:"),
        Line::from("  Drag      Click and drag to select text"),
        Line::from("  y         Yank (copy) selection to clipboard"),
        Line::from("  Esc       Clear selection"),
        Line::from(""),
        Line::from("  q         Quit"),
        Line::from("  ?         Toggle help"),
    ];

    let total_lines = help_text.len();
    let scroll = app.help_scroll.min(total_lines.saturating_sub(visible_height));

    // Build title with scroll indicator
    let title = if total_lines > visible_height {
        format!(
            " Help [{}/{}] ",
            scroll + 1,
            total_lines.saturating_sub(visible_height) + 1
        )
    } else {
        " Help ".to_string()
    };

    let paragraph = Paragraph::new(help_text)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .style(Style::default().bg(Color::Black)),
        )
        .scroll((scroll as u16, 0));

    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(paragraph, area);
}

/// Draw export dialog overlay.
fn draw_export_dialog(f: &mut Frame, app: &AppState) {
    let area = centered_rect(50, 40, f.area());

    // Build dialog content
    let mut lines = vec![
        Line::from(Span::styled(
            "Export Session",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    // Format selection with arrows
    let format_line = Line::from(vec![
        Span::raw("Format: "),
        Span::raw("◀ "),
        Span::styled(
            app.export_dialog.format_name(),
            Style::default().fg(app.theme.primary).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ▶"),
    ]);
    lines.push(format_line);
    lines.push(Line::from(""));

    // Toggles
    let thinking_checkbox = if app.export_dialog.include_thinking { "[x]" } else { "[ ]" };
    let tools_checkbox = if app.export_dialog.include_tools { "[x]" } else { "[ ]" };

    lines.push(Line::from(format!(
        "{} Include thinking blocks (t)",
        thinking_checkbox
    )));
    lines.push(Line::from(format!(
        "{} Include tool outputs (o)",
        tools_checkbox
    )));
    lines.push(Line::from(""));

    // Status message
    if let Some(msg) = &app.export_dialog.status_message {
        let style = if msg.starts_with("Error") {
            Style::default().fg(app.theme.error)
        } else {
            Style::default().fg(app.theme.success)
        };
        lines.push(Line::from(Span::styled(msg.clone(), style)));
        lines.push(Line::from(""));
    }

    // Instructions
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Enter: Export  |  h/l: Change format  |  Esc: Cancel",
        Style::default().fg(Color::DarkGray),
    )));

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(" Export ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.theme.primary))
            .style(Style::default().bg(Color::Black)),
    );

    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(paragraph, area);
}

/// Render selection overlay on a panel.
/// This modifies the buffer to highlight selected text.
fn render_selection_overlay(f: &mut Frame, app: &AppState, area: Rect, panel: usize) {
    // Only render if this panel has the selection
    if app.selection_panel != Some(panel) {
        return;
    }
    if !app.has_selection() {
        return;
    }

    let Some(((start_col, start_row), (end_col, end_row))) = app.get_selection_range() else {
        return;
    };

    // Get buffer for direct cell modification
    let buf = f.buffer_mut();
    let style = selection_style();

    // Iterate through selection range and highlight cells
    for row in start_row..=end_row {
        // Skip rows outside the area
        if row < area.y || row >= area.y + area.height {
            continue;
        }

        // Determine column range for this row
        let col_start = if row == start_row { start_col } else { area.x };
        let col_end = if row == end_row { end_col } else { area.x + area.width.saturating_sub(1) };

        // Clamp to area bounds
        let col_start = col_start.max(area.x);
        let col_end = col_end.min(area.x + area.width.saturating_sub(1));

        for col in col_start..=col_end {
            // Ensure we're within buffer bounds
            if col < buf.area.x
                || col >= buf.area.x + buf.area.width
                || row < buf.area.y
                || row >= buf.area.y + buf.area.height
            {
                continue;
            }

            // Get the cell and apply selection style
            let cell = buf.cell_mut((col, row));
            if let Some(cell) = cell {
                cell.set_style(style);
            }
        }
    }
}

/// Create a centered rectangle.
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

/// Draw command palette overlay.
fn draw_command_palette(f: &mut Frame, app: &AppState) {
    // Use 50% width, 60% height for the palette
    let area = centered_rect(50, 60, f.area());

    // Build the search input line
    let search_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(app.theme.primary)),
        Span::raw(&app.command_palette.query),
        Span::styled("█", Style::default().fg(app.theme.primary)),
    ]);

    // Build list of filtered commands
    let mut lines: Vec<Line> = vec![search_line, Line::from("")];

    // Calculate visible range (show ~10 commands max)
    let max_visible = (area.height as usize).saturating_sub(6).min(15);
    let filtered_count = app.command_palette.filtered.len();
    let selected = app.command_palette.selected;

    // Calculate scroll offset to keep selected item visible
    let scroll_offset = if selected >= max_visible {
        selected - max_visible + 1
    } else {
        0
    };

    for (display_idx, &cmd_idx) in app
        .command_palette
        .filtered
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(max_visible)
    {
        let cmd = &app.command_palette.commands[cmd_idx];
        let is_selected = display_idx == selected;

        let (name_style, desc_style, shortcut_style) = if is_selected {
            (
                Style::default().fg(Color::Black).bg(app.theme.primary).add_modifier(Modifier::BOLD),
                Style::default().fg(Color::Black).bg(app.theme.primary),
                Style::default().fg(Color::DarkGray).bg(app.theme.primary),
            )
        } else {
            (
                Style::default().fg(app.theme.primary),
                Style::default().fg(Color::Gray),
                Style::default().fg(Color::DarkGray),
            )
        };

        // Build the line with name, description, and shortcut
        let prefix = if is_selected { "▶ " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(prefix, name_style),
            Span::styled(cmd.name, name_style),
            Span::styled(" - ", desc_style),
            Span::styled(cmd.description, desc_style),
        ]));
        lines.push(Line::from(vec![
            Span::styled("    ", shortcut_style),
            Span::styled(format!("[{}]", cmd.shortcut), shortcut_style),
        ]));
    }

    // Show scroll indicator if needed
    if filtered_count > max_visible {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  ... {} of {} commands", max_visible.min(filtered_count), filtered_count),
            Style::default().fg(Color::DarkGray),
        )));
    }

    // Add instructions at the bottom
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "↑/↓: Navigate  |  Enter: Execute  |  Esc: Close",
        Style::default().fg(Color::DarkGray),
    )));

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(" Command Palette (Ctrl+P) ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.theme.primary))
            .style(Style::default().bg(Color::Black)),
    );

    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(paragraph, area);
}
