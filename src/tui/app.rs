//! TUI application main loop.

use std::io;

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, KeyCode, KeyModifiers},
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

use crate::error::{Result, SnatchError};

use super::components::{ScrollableText, StatusBar};
use super::events::{Event, EventHandler, KeyBindings};
use super::state::AppState;
use super::theme::available_themes;

/// Run the TUI application.
pub fn run(project: Option<&str>, session: Option<&str>) -> Result<()> {
    // Setup terminal
    enable_raw_mode().map_err(|e| SnatchError::io("Failed to enable raw mode", e))?;

    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .map_err(|e| SnatchError::io("Failed to enter alternate screen", e))?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)
        .map_err(|e| SnatchError::io("Failed to create terminal", e))?;

    // Create app state
    let mut app = AppState::new()?;

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
fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut AppState,
) -> Result<()> {
    // Create event handler with 100ms tick rate
    let events = EventHandler::new(std::time::Duration::from_millis(100));
    let bindings = KeyBindings::default();

    loop {
        // Draw UI
        terminal.draw(|f| draw_ui(f, app))?;

        // Handle events from the event handler
        match events.next() {
            Ok(Event::Key(key)) => {
                // Check configurable key bindings first
                if bindings.is_quit(&key) {
                    return Ok(());
                }

                if bindings.is_up(&key) {
                    app.previous();
                    continue;
                }

                if bindings.is_down(&key) {
                    app.next();
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

                    // Refresh
                    (KeyModifiers::NONE, KeyCode::Char('r')) => {
                        app.refresh()?;
                    }

                    // Export
                    (KeyModifiers::NONE, KeyCode::Char('e')) => {
                        app.export()?;
                    }

                    // Toggle thinking
                    (KeyModifiers::NONE, KeyCode::Char('t')) => {
                        app.toggle_thinking();
                    }

                    // Toggle tools
                    (KeyModifiers::NONE, KeyCode::Char('o')) => {
                        app.toggle_tools();
                    }

                    // Cycle theme
                    (KeyModifiers::NONE, KeyCode::Char('T')) => {
                        app.cycle_theme();
                    }

                    // Help
                    (KeyModifiers::NONE, KeyCode::Char('?')) => {
                        app.toggle_help();
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
            Ok(Event::Mouse(_)) => {
                // Mouse events - could add mouse support later
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
    // Main layout: content area + status bar
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(f.area());

    // Content area: three columns
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

    // Center panel: Conversation
    draw_conversation_panel(f, app, chunks[1]);

    // Right panel: Details
    draw_details_panel(f, app, chunks[2]);

    // Status bar at bottom
    draw_status_bar(f, app, main_chunks[1]);

    // Help overlay if active
    if app.show_help {
        draw_help_overlay(f);
    }
}

/// Draw the tree panel (projects and sessions).
fn draw_tree_panel(f: &mut Frame, app: &AppState, area: Rect) {
    let border_style = if app.focus == 0 {
        app.theme.border_focused_style()
    } else {
        app.theme.border_style()
    };

    let items: Vec<ListItem> = app
        .tree_items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let style = if Some(i) == app.tree_selected {
                app.theme.selection_style()
            } else {
                Style::default()
            };
            ListItem::new(item.clone()).style(style)
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
    } else {
        app.conversation_lines
            .iter()
            .skip(app.scroll_offset)
            .take(area.height as usize - 2)
            .cloned()
            .collect()
    };

    let paragraph = Paragraph::new(content)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(border_style),
        )
        .wrap(Wrap { trim: false });

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
    let mode = if app.show_help {
        "HELP"
    } else {
        match app.focus {
            0 => "TREE",
            1 => "CONVERSATION",
            2 => "DETAILS",
            _ => "UNKNOWN",
        }
    };

    let left_content = vec![
        Span::styled(" snatch ", Style::default().fg(app.theme.primary).add_modifier(Modifier::BOLD)),
        Span::raw("│ "),
        Span::styled(mode, Style::default().fg(app.theme.warning)),
        Span::raw(" │ "),
        Span::styled(&app.theme.name, Style::default().fg(app.theme.secondary)),
    ];

    let right_content = if let Some(session_id) = &app.current_session {
        let short_id = &session_id[..8.min(session_id.len())];
        vec![
            Span::raw(format!("{} msgs ", app.conversation_lines.len())),
            Span::raw("│ "),
            Span::raw(format!("Session: {short_id} ")),
        ]
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

/// Draw help overlay.
fn draw_help_overlay(f: &mut Frame) {
    let area = centered_rect(60, 60, f.area());

    let help_text = vec![
        Line::from(Span::styled("Keyboard Shortcuts", Style::default().add_modifier(Modifier::BOLD))),
        Line::from(""),
        Line::from("Navigation:"),
        Line::from("  j/↓       Move down"),
        Line::from("  k/↑       Move up"),
        Line::from("  h/←       Focus left panel"),
        Line::from("  l/→       Focus right panel"),
        Line::from("  Enter     Select/expand"),
        Line::from("  Esc       Go back"),
        Line::from(""),
        Line::from("Panels:"),
        Line::from("  1         Focus tree panel"),
        Line::from("  2         Focus conversation panel"),
        Line::from("  3         Focus details panel"),
        Line::from(""),
        Line::from("Actions:"),
        Line::from("  r         Refresh"),
        Line::from("  e         Export session"),
        Line::from("  t         Toggle thinking blocks"),
        Line::from("  o         Toggle tool outputs"),
        Line::from(format!("  T         Cycle theme ({})", available_themes().join("/"))),
        Line::from("  /         Search"),
        Line::from(""),
        Line::from("  q         Quit"),
        Line::from("  ?         Toggle help"),
    ];

    let paragraph = Paragraph::new(help_text)
        .block(
            Block::default()
                .title(" Help ")
                .borders(Borders::ALL)
                .style(Style::default().bg(Color::Black)),
        );

    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(paragraph, area);
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
