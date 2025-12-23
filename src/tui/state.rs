//! TUI application state.

use ratatui::text::Line;

use crate::analytics::SessionAnalytics;
use crate::discovery::{ClaudeDirectory, Project, Session};
use crate::error::Result;
use crate::model::{ContentBlock, LogEntry};
use crate::reconstruction::Conversation;

use super::components::{format_message_header, MessageType};
use super::theme::Theme;

/// Application state.
pub struct AppState {
    /// Claude directory.
    pub claude_dir: ClaudeDirectory,
    /// All projects.
    pub projects: Vec<Project>,
    /// Current project index.
    pub current_project: Option<usize>,
    /// Current session ID.
    pub current_session: Option<String>,
    /// Currently focused panel (0=tree, 1=conversation, 2=details).
    pub focus: usize,
    /// Tree view items.
    pub tree_items: Vec<String>,
    /// Selected item in tree.
    pub tree_selected: Option<usize>,
    /// Conversation display lines.
    pub conversation_lines: Vec<Line<'static>>,
    /// Details panel lines.
    pub details_lines: Vec<Line<'static>>,
    /// Scroll offset for conversation.
    pub scroll_offset: usize,
    /// Scroll offset for details panel.
    pub details_scroll: usize,
    /// Show help overlay.
    pub show_help: bool,
    /// Show thinking blocks.
    pub show_thinking: bool,
    /// Show tool outputs.
    pub show_tools: bool,
    /// Current theme.
    pub theme: Theme,
    /// Current entries (for navigation).
    entries: Vec<LogEntry>,
}

impl AppState {
    /// Create new app state.
    pub fn new() -> Result<Self> {
        let claude_dir = ClaudeDirectory::discover()?;
        let projects = claude_dir.projects()?;

        let tree_items: Vec<String> = projects
            .iter()
            .map(|p| p.decoded_path().to_string())
            .collect();

        let tree_selected = if tree_items.is_empty() { None } else { Some(0) };

        Ok(Self {
            claude_dir,
            projects,
            current_project: None,
            current_session: None,
            focus: 0,
            tree_items,
            tree_selected,
            conversation_lines: Vec::new(),
            details_lines: Vec::new(),
            scroll_offset: 0,
            details_scroll: 0,
            show_help: false,
            show_thinking: true,
            show_tools: true,
            theme: Theme::default(),
            entries: Vec::new(),
        })
    }

    /// Select a session by ID.
    pub fn select_session(&mut self, session_id: &str) -> Result<()> {
        if let Some(session) = self.claude_dir.find_session(session_id)? {
            self.load_session(&session)?;
        }
        Ok(())
    }

    /// Select a project by path.
    pub fn select_project(&mut self, project_path: &str) -> Result<()> {
        for (i, project) in self.projects.iter().enumerate() {
            if project.decoded_path().contains(project_path) {
                self.current_project = Some(i);
                self.update_tree_for_project(i)?;
                break;
            }
        }
        Ok(())
    }

    /// Move selection up.
    pub fn previous(&mut self) {
        if let Some(selected) = self.tree_selected {
            if selected > 0 {
                self.tree_selected = Some(selected - 1);
            }
        }
    }

    /// Move selection down.
    pub fn next(&mut self) {
        if let Some(selected) = self.tree_selected {
            if selected + 1 < self.tree_items.len() {
                self.tree_selected = Some(selected + 1);
            }
        }
    }

    /// Focus left panel.
    pub fn focus_left(&mut self) {
        if self.focus > 0 {
            self.focus -= 1;
        }
    }

    /// Focus right panel.
    pub fn focus_right(&mut self) {
        if self.focus < 2 {
            self.focus += 1;
        }
    }

    /// Set focus to specific panel.
    pub fn set_focus(&mut self, panel: usize) {
        if panel <= 2 {
            self.focus = panel;
        }
    }

    /// Select current item.
    pub fn select(&mut self) -> Result<()> {
        if let Some(selected) = self.tree_selected {
            if self.current_project.is_none() {
                // Selecting a project
                self.current_project = Some(selected);
                self.update_tree_for_project(selected)?;
            } else {
                // Selecting a session
                let project_idx = self.current_project.unwrap();
                if let Some(project) = self.projects.get(project_idx) {
                    let sessions = project.sessions()?;
                    if let Some(session) = sessions.get(selected) {
                        self.load_session(session)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Go back to previous view.
    pub fn back(&mut self) {
        if self.current_session.is_some() {
            self.current_session = None;
            self.conversation_lines.clear();
            self.entries.clear();
        } else if self.current_project.is_some() {
            self.current_project = None;
            self.tree_items = self.projects
                .iter()
                .map(|p| p.decoded_path().to_string())
                .collect();
            self.tree_selected = Some(0);
        }
    }

    /// Scroll up.
    pub fn scroll_up(&mut self, amount: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
    }

    /// Scroll down.
    pub fn scroll_down(&mut self, amount: usize) {
        let max_scroll = self.conversation_lines.len().saturating_sub(10);
        self.scroll_offset = (self.scroll_offset + amount).min(max_scroll);
    }

    /// Scroll to top.
    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
    }

    /// Scroll to bottom.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = self.conversation_lines.len().saturating_sub(10);
    }

    /// Start search mode.
    pub fn start_search(&mut self) {
        // TODO: Implement search mode
    }

    /// Refresh current view.
    pub fn refresh(&mut self) -> Result<()> {
        self.projects = self.claude_dir.projects()?;
        if let Some(session_id) = &self.current_session.clone() {
            if let Some(session) = self.claude_dir.find_session(session_id)? {
                self.load_session(&session)?;
            }
        }
        Ok(())
    }

    /// Export current session.
    pub fn export(&mut self) -> Result<()> {
        // TODO: Implement export dialog
        Ok(())
    }

    /// Toggle thinking blocks.
    pub fn toggle_thinking(&mut self) {
        self.show_thinking = !self.show_thinking;
        self.update_conversation_display();
    }

    /// Toggle tool outputs.
    pub fn toggle_tools(&mut self) {
        self.show_tools = !self.show_tools;
        self.update_conversation_display();
    }

    /// Toggle help.
    pub fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    /// Cycle through available themes.
    pub fn cycle_theme(&mut self) {
        self.theme = match self.theme.name.as_str() {
            "dark" => Theme::light(),
            "light" => Theme::high_contrast(),
            _ => Theme::dark(),
        };
    }

    /// Update tree for project.
    fn update_tree_for_project(&mut self, project_idx: usize) -> Result<()> {
        if let Some(project) = self.projects.get(project_idx) {
            let sessions = project.sessions()?;
            self.tree_items = sessions
                .iter()
                .map(|s| {
                    let id = &s.session_id()[..8.min(s.session_id().len())];
                    let subagent = if s.is_subagent() { " [sub]" } else { "" };
                    format!("{id}{subagent}")
                })
                .collect();
            self.tree_selected = if self.tree_items.is_empty() {
                None
            } else {
                Some(0)
            };
        }
        Ok(())
    }

    /// Load a session.
    fn load_session(&mut self, session: &Session) -> Result<()> {
        self.current_session = Some(session.session_id().to_string());
        self.entries = session.parse()?;

        // Build conversation
        let conversation = Conversation::from_entries(self.entries.clone())?;

        // Update details
        let analytics = SessionAnalytics::from_conversation(&conversation);
        let summary = analytics.summary_report();

        self.details_lines = vec![
            Line::from(format!("Session: {}", session.session_id())),
            Line::from(""),
            Line::from(format!("Messages: {}", summary.total_messages)),
            Line::from(format!("  User: {}", summary.user_messages)),
            Line::from(format!("  Assistant: {}", summary.assistant_messages)),
            Line::from(""),
            Line::from(format!("Tokens: {}", summary.total_tokens)),
            Line::from(format!("  Input: {}", summary.input_tokens)),
            Line::from(format!("  Output: {}", summary.output_tokens)),
            Line::from(""),
            Line::from(format!("Tools: {}", summary.tool_invocations)),
            Line::from(format!("Thinking: {}", summary.thinking_blocks)),
            Line::from(""),
            Line::from(format!("Duration: {}", summary.duration_string())),
            Line::from(format!("Cost: {}", summary.cost_string())),
        ];

        self.update_conversation_display();
        self.scroll_offset = 0;
        self.focus = 1; // Focus conversation panel

        Ok(())
    }

    /// Update conversation display based on current settings.
    fn update_conversation_display(&mut self) {
        self.conversation_lines.clear();

        for entry in &self.entries {
            match entry {
                LogEntry::User(user) => {
                    // Use formatted message header with timestamp
                    let timestamp = user.timestamp.format("%H:%M:%S").to_string();
                    self.conversation_lines.push(format_message_header(
                        MessageType::User,
                        Some(&timestamp),
                    ));
                    self.conversation_lines.push(Line::from("────────────────────────────────────────"));

                    let text = match &user.message {
                        crate::model::UserContent::Simple(s) => s.content.clone(),
                        crate::model::UserContent::Blocks(b) => {
                            b.content.iter().filter_map(|c| {
                                match c {
                                    crate::model::ContentBlock::Text(t) => Some(t.text.clone()),
                                    _ => None,
                                }
                            }).collect::<Vec<_>>().join("\n")
                        }
                    };
                    for line in text.lines() {
                        self.conversation_lines.push(Line::from(line.to_string()));
                    }
                    self.conversation_lines.push(Line::from(""));
                }
                LogEntry::Assistant(assistant) => {
                    // Use formatted message header with timestamp
                    let timestamp = assistant.timestamp.format("%H:%M:%S").to_string();
                    self.conversation_lines.push(format_message_header(
                        MessageType::Assistant,
                        Some(&timestamp),
                    ));
                    self.conversation_lines.push(Line::from("────────────────────────────────────────"));

                    for block in &assistant.message.content {
                        match block {
                            ContentBlock::Text(text) => {
                                for line in text.text.lines() {
                                    self.conversation_lines.push(Line::from(line.to_string()));
                                }
                            }
                            ContentBlock::Thinking(thinking) if self.show_thinking => {
                                self.conversation_lines.push(Line::from("┌─ 💭 Thinking ─────────────────────────"));
                                for line in thinking.thinking.lines().take(10) {
                                    self.conversation_lines.push(Line::from(format!("│ {line}")));
                                }
                                if thinking.thinking.lines().count() > 10 {
                                    self.conversation_lines.push(Line::from("│ ..."));
                                }
                                self.conversation_lines.push(Line::from("└───────────────────────────────────────"));
                            }
                            ContentBlock::ToolUse(tool) if self.show_tools => {
                                self.conversation_lines.push(format_message_header(
                                    MessageType::Tool,
                                    None,
                                ));
                                self.conversation_lines.push(Line::from(format!(
                                    "   {} ({})",
                                    tool.name,
                                    &tool.id[..8.min(tool.id.len())]
                                )));
                            }
                            ContentBlock::ToolResult(result) if self.show_tools => {
                                let status = if result.is_explicit_error() { "❌" } else { "✓" };
                                self.conversation_lines.push(Line::from(format!(
                                    "   {status} Result for {}",
                                    &result.tool_use_id[..8.min(result.tool_use_id.len())]
                                )));
                            }
                            _ => {}
                        }
                    }
                    self.conversation_lines.push(Line::from(""));
                }
                LogEntry::System(system) => {
                    self.conversation_lines.push(format_message_header(
                        MessageType::System,
                        None,
                    ));
                    if let Some(subtype) = &system.subtype {
                        self.conversation_lines.push(Line::from(format!("   {subtype:?}")));
                    }
                }
                LogEntry::Summary(summary) => {
                    self.conversation_lines.push(format_message_header(
                        MessageType::Summary,
                        None,
                    ));
                    self.conversation_lines.push(Line::from("────────────────────────────────────────"));
                    let text = &summary.summary;
                    for line in text.lines().take(5) {
                        self.conversation_lines.push(Line::from(line.to_string()));
                    }
                    if text.lines().count() > 5 {
                        self.conversation_lines.push(Line::from("..."));
                    }
                    self.conversation_lines.push(Line::from(""));
                }
                _ => {}
            }
        }
    }
}
