//! TUI application state.

use std::fs::File;
use std::io::BufWriter;

use ratatui::text::Line;

use crate::analytics::SessionAnalytics;
use crate::discovery::{ClaudeDirectory, HierarchyBuilder, Project, Session};
use crate::error::Result;
use crate::export::{
    CsvExporter, ExportFormat, ExportOptions, Exporter, HtmlExporter, JsonExporter,
    MarkdownExporter, SqliteExporter, TextExporter, XmlExporter,
};
use crate::model::{ContentBlock, LogEntry};
use crate::parser::JsonlParser;
use crate::reconstruction::Conversation;

use super::components::{format_message_header, MessageType};
use super::highlight::SyntaxHighlighter;
use super::theme::Theme;

/// Search mode state.
#[derive(Debug, Clone, Default)]
pub struct SearchState {
    /// Whether search mode is active.
    pub active: bool,
    /// Current search query.
    pub query: String,
    /// Cursor position in search input.
    pub cursor: usize,
    /// Search results (line indices).
    pub results: Vec<usize>,
    /// Current result index.
    pub current_result: usize,
    /// Whether search is case-insensitive.
    pub case_insensitive: bool,
}

impl SearchState {
    /// Clear search state.
    pub fn clear(&mut self) {
        self.active = false;
        self.query.clear();
        self.cursor = 0;
        self.results.clear();
        self.current_result = 0;
    }

    /// Add a character to the search query.
    pub fn push_char(&mut self, c: char) {
        self.query.insert(self.cursor, c);
        self.cursor += 1;
    }

    /// Remove character before cursor.
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.query.remove(self.cursor);
        }
    }

    /// Move to next search result.
    pub fn next_result(&mut self) {
        if !self.results.is_empty() {
            self.current_result = (self.current_result + 1) % self.results.len();
        }
    }

    /// Move to previous search result.
    pub fn prev_result(&mut self) {
        if !self.results.is_empty() {
            self.current_result = if self.current_result == 0 {
                self.results.len() - 1
            } else {
                self.current_result - 1
            };
        }
    }

    /// Get current result line index.
    #[must_use]
    pub fn current_line(&self) -> Option<usize> {
        self.results.get(self.current_result).copied()
    }

    /// Get result count display string.
    #[must_use]
    pub fn result_count_str(&self) -> String {
        if self.results.is_empty() {
            "No matches".to_string()
        } else {
            format!("{}/{}", self.current_result + 1, self.results.len())
        }
    }
}

/// Export dialog state.
#[derive(Debug, Clone)]
pub struct ExportDialogState {
    /// Whether the dialog is active.
    pub active: bool,
    /// Selected format index.
    pub format_index: usize,
    /// Available formats.
    pub formats: Vec<ExportFormat>,
    /// Include thinking blocks.
    pub include_thinking: bool,
    /// Include tool outputs.
    pub include_tools: bool,
    /// Status message (success/error).
    pub status_message: Option<String>,
    /// Whether export is in progress.
    pub exporting: bool,
}

impl Default for ExportDialogState {
    fn default() -> Self {
        Self {
            active: false,
            format_index: 0,
            formats: vec![
                ExportFormat::Markdown,
                ExportFormat::Json,
                ExportFormat::JsonPretty,
                ExportFormat::Html,
                ExportFormat::Text,
            ],
            include_thinking: true,
            include_tools: true,
            status_message: None,
            exporting: false,
        }
    }
}

impl ExportDialogState {
    /// Get the currently selected format.
    #[must_use]
    pub fn selected_format(&self) -> ExportFormat {
        self.formats[self.format_index]
    }

    /// Move to the next format.
    pub fn next_format(&mut self) {
        self.format_index = (self.format_index + 1) % self.formats.len();
    }

    /// Move to the previous format.
    pub fn prev_format(&mut self) {
        if self.format_index == 0 {
            self.format_index = self.formats.len() - 1;
        } else {
            self.format_index -= 1;
        }
    }

    /// Clear the dialog state.
    pub fn clear(&mut self) {
        self.active = false;
        self.status_message = None;
        self.exporting = false;
    }

    /// Format name for display.
    #[must_use]
    pub fn format_name(&self) -> &'static str {
        match self.selected_format() {
            ExportFormat::Markdown => "Markdown",
            ExportFormat::Json => "JSON",
            ExportFormat::JsonPretty => "JSON (Pretty)",
            ExportFormat::Html => "HTML",
            ExportFormat::Text => "Plain Text",
            ExportFormat::Csv => "CSV",
            ExportFormat::Xml => "XML",
            ExportFormat::Sqlite => "SQLite",
        }
    }
}

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
    /// Enable word wrap in conversation panel.
    pub word_wrap: bool,
    /// Current theme.
    pub theme: Theme,
    /// Current entries (for navigation).
    entries: Vec<LogEntry>,
    /// Search mode state.
    pub search_state: SearchState,
    /// Export dialog state.
    pub export_dialog: ExportDialogState,
    /// Syntax highlighter for code blocks.
    highlighter: SyntaxHighlighter,
    /// Status message to display.
    pub status_message: Option<String>,
    /// Mapping from tree item index to session ID (for session view).
    tree_session_ids: Vec<String>,
}

impl AppState {
    /// Create new app state with the default theme.
    #[allow(dead_code)]
    pub fn new() -> Result<Self> {
        Self::with_theme(None)
    }

    /// Create new app state with a specific theme.
    pub fn with_theme(theme_name: Option<&str>) -> Result<Self> {
        let claude_dir = ClaudeDirectory::discover()?;
        let projects = claude_dir.projects()?;

        let tree_items: Vec<String> = projects
            .iter()
            .map(|p| p.decoded_path().to_string())
            .collect();

        let tree_selected = if tree_items.is_empty() { None } else { Some(0) };

        // Load theme by name or use default
        let theme = theme_name
            .and_then(Theme::from_name)
            .unwrap_or_default();

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
            word_wrap: true,
            theme,
            entries: Vec::new(),
            search_state: SearchState::default(),
            export_dialog: ExportDialogState::default(),
            highlighter: SyntaxHighlighter::new(),
            status_message: None,
            tree_session_ids: Vec::new(),
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
                // Selecting a session from hierarchical tree
                if let Some(session_id) = self.tree_session_ids.get(selected).cloned() {
                    // Find the session by ID
                    if let Some(session) = self.claude_dir.find_session(&session_id)? {
                        self.load_session(&session)?;
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
        self.search_state.active = true;
        self.search_state.query.clear();
        self.search_state.cursor = 0;
        self.search_state.results.clear();
        self.search_state.current_result = 0;
    }

    /// Cancel search mode.
    pub fn cancel_search(&mut self) {
        self.search_state.clear();
    }

    /// Handle character input during search.
    pub fn search_input(&mut self, c: char) {
        self.search_state.push_char(c);
        self.perform_search();
    }

    /// Handle backspace during search.
    pub fn search_backspace(&mut self) {
        self.search_state.backspace();
        self.perform_search();
    }

    /// Move to next search result.
    pub fn search_next(&mut self) {
        self.search_state.next_result();
        if let Some(line) = self.search_state.current_line() {
            self.scroll_to_line(line);
        }
    }

    /// Move to previous search result.
    pub fn search_prev(&mut self) {
        self.search_state.prev_result();
        if let Some(line) = self.search_state.current_line() {
            self.scroll_to_line(line);
        }
    }

    /// Perform search on conversation lines.
    fn perform_search(&mut self) {
        self.search_state.results.clear();
        self.search_state.current_result = 0;

        if self.search_state.query.is_empty() {
            return;
        }

        let query = if self.search_state.case_insensitive {
            self.search_state.query.to_lowercase()
        } else {
            self.search_state.query.clone()
        };

        for (i, line) in self.conversation_lines.iter().enumerate() {
            let line_text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            let line_to_search = if self.search_state.case_insensitive {
                line_text.to_lowercase()
            } else {
                line_text
            };

            if line_to_search.contains(&query) {
                self.search_state.results.push(i);
            }
        }

        // Jump to first result
        if let Some(line) = self.search_state.current_line() {
            self.scroll_to_line(line);
        }
    }

    /// Scroll to a specific line.
    fn scroll_to_line(&mut self, line: usize) {
        // Center the line in the viewport (approximately)
        self.scroll_offset = line.saturating_sub(10);
    }

    /// Check if search mode is active.
    #[must_use]
    pub fn is_searching(&self) -> bool {
        self.search_state.active
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

    /// Check if export dialog is active.
    #[must_use]
    pub fn is_exporting(&self) -> bool {
        self.export_dialog.active
    }

    /// Open the export dialog.
    pub fn export(&mut self) -> Result<()> {
        if self.current_session.is_none() {
            return Ok(());
        }
        self.export_dialog.active = true;
        self.export_dialog.status_message = None;
        // Sync with current display settings
        self.export_dialog.include_thinking = self.show_thinking;
        self.export_dialog.include_tools = self.show_tools;
        Ok(())
    }

    /// Cancel the export dialog.
    pub fn cancel_export(&mut self) {
        self.export_dialog.clear();
    }

    /// Perform the actual export.
    pub fn confirm_export(&mut self) -> Result<()> {
        let Some(session_id) = &self.current_session.clone() else {
            self.export_dialog.status_message = Some("No session selected".to_string());
            return Ok(());
        };

        let Some(session) = self.claude_dir.find_session(session_id)? else {
            self.export_dialog.status_message = Some("Session not found".to_string());
            return Ok(());
        };

        self.export_dialog.exporting = true;

        // Parse the session
        let mut parser = JsonlParser::new();
        let entries = parser.parse_file(session.path())?;
        let conversation = Conversation::from_entries(entries)?;

        // Build export options
        let options = ExportOptions::default()
            .with_thinking(self.export_dialog.include_thinking)
            .with_tool_use(self.export_dialog.include_tools);

        // Determine output path
        let format = self.export_dialog.selected_format();
        let extension = format.extension();
        let output_path = std::env::current_dir()
            .unwrap_or_default()
            .join(format!("session_{}.{}", &session_id[..8.min(session_id.len())], extension));

        // Create output file
        let file = File::create(&output_path)?;
        let mut writer = BufWriter::new(file);

        // Export based on format
        match format {
            ExportFormat::Markdown => {
                let exporter = MarkdownExporter::new();
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
            ExportFormat::Json | ExportFormat::JsonPretty => {
                let exporter = JsonExporter::new()
                    .pretty(matches!(format, ExportFormat::JsonPretty));
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
            ExportFormat::Html => {
                let exporter = HtmlExporter::new()
                    .dark_theme(true);
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
            ExportFormat::Text => {
                let exporter = TextExporter::new();
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
            ExportFormat::Csv => {
                let exporter = CsvExporter::new();
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
            ExportFormat::Xml => {
                let exporter = XmlExporter::new();
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
            ExportFormat::Sqlite => {
                // SQLite requires direct file access
                drop(writer);
                std::fs::remove_file(&output_path).ok();
                let exporter = SqliteExporter::new();
                exporter.export_to_file(&conversation, &output_path, &options)?;
            }
        }

        self.export_dialog.exporting = false;
        self.export_dialog.status_message = Some(format!("Exported to: {}", output_path.display()));

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

    /// Toggle word wrap.
    pub fn toggle_word_wrap(&mut self) {
        self.word_wrap = !self.word_wrap;
        if self.word_wrap {
            self.status_message = Some("Word wrap: ON".to_string());
        } else {
            self.status_message = Some("Word wrap: OFF".to_string());
        }
    }

    /// Cycle through available themes.
    pub fn cycle_theme(&mut self) {
        self.theme = match self.theme.name.as_str() {
            "dark" => Theme::light(),
            "light" => Theme::high_contrast(),
            _ => Theme::dark(),
        };
    }

    /// Copy current message to clipboard.
    pub fn copy_message(&mut self) -> Result<()> {
        let text = self.get_current_message_text();
        if text.is_empty() {
            self.status_message = Some("No message to copy".to_string());
            return Ok(());
        }

        match arboard::Clipboard::new() {
            Ok(mut clipboard) => {
                if let Err(e) = clipboard.set_text(&text) {
                    self.status_message = Some(format!("Clipboard error: {e}"));
                } else {
                    let len = text.len();
                    self.status_message = Some(format!("Copied {len} characters to clipboard"));
                }
            }
            Err(e) => {
                self.status_message = Some(format!("Clipboard not available: {e}"));
            }
        }
        Ok(())
    }

    /// Get text of current message.
    fn get_current_message_text(&self) -> String {
        // Get the currently viewed message based on scroll position
        if self.conversation_lines.is_empty() {
            return String::new();
        }

        // For simplicity, copy all visible content
        self.conversation_lines
            .iter()
            .map(|line| {
                line.spans.iter()
                    .map(|span| span.content.to_string())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Copy code block at current position to clipboard.
    pub fn copy_code_block(&mut self) -> Result<()> {
        // Extract code blocks from current content
        let code_blocks = self.extract_code_blocks();

        if code_blocks.is_empty() {
            self.status_message = Some("No code blocks found".to_string());
            return Ok(());
        }

        // Copy the first code block (or the one near current scroll position)
        let text = &code_blocks[0];

        match arboard::Clipboard::new() {
            Ok(mut clipboard) => {
                if let Err(e) = clipboard.set_text(text) {
                    self.status_message = Some(format!("Clipboard error: {e}"));
                } else {
                    let lines = text.lines().count();
                    self.status_message = Some(format!("Copied code block ({lines} lines)"));
                }
            }
            Err(e) => {
                self.status_message = Some(format!("Clipboard not available: {e}"));
            }
        }
        Ok(())
    }

    /// Extract code blocks from content.
    fn extract_code_blocks(&self) -> Vec<String> {
        let mut blocks = Vec::new();
        let mut in_block = false;
        let mut current_block = String::new();

        let full_text = self.get_current_message_text();

        for line in full_text.lines() {
            if line.starts_with("```") {
                if in_block {
                    // End of block
                    blocks.push(current_block.clone());
                    current_block.clear();
                    in_block = false;
                } else {
                    // Start of block
                    in_block = true;
                }
            } else if in_block {
                if !current_block.is_empty() {
                    current_block.push('\n');
                }
                current_block.push_str(line);
            }
        }

        blocks
    }

    /// Update tree for project.
    fn update_tree_for_project(&mut self, project_idx: usize) -> Result<()> {
        if let Some(project) = self.projects.get(project_idx) {
            // Build hierarchical view of sessions
            let hierarchy = HierarchyBuilder::new().build_for_project(project)?;

            self.tree_items.clear();
            self.tree_session_ids.clear();

            for node in hierarchy {
                self.add_hierarchy_node_to_tree(&node);
            }

            self.tree_selected = if self.tree_items.is_empty() {
                None
            } else {
                Some(0)
            };
        }
        Ok(())
    }

    /// Add a hierarchy node to the tree (recursively).
    fn add_hierarchy_node_to_tree(&mut self, node: &crate::discovery::AgentNode) {
        let id = node.session.session_id();
        let short_id = &id[..8.min(id.len())];
        let indent = "  ".repeat(node.depth);

        let label = if node.session.is_subagent() {
            format!("{indent}└─ {short_id} [agent]")
        } else if !node.children.is_empty() {
            format!("{indent}{short_id} ({} agents)", node.children.len())
        } else {
            format!("{indent}{short_id}")
        };

        self.tree_items.push(label);
        self.tree_session_ids.push(id.to_string());

        // Add children (subagents) indented
        for child in &node.children {
            self.add_hierarchy_node_to_tree(child);
        }
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
                    // Use syntax highlighting for user text
                    let highlighted = self.highlighter.highlight_markdown_text(&text);
                    self.conversation_lines.extend(highlighted);
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
                                // Use syntax highlighting for assistant text
                                let highlighted = self.highlighter.highlight_markdown_text(&text.text);
                                self.conversation_lines.extend(highlighted);
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
