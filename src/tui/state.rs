//! TUI application state.

use std::collections::HashSet;
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
use crate::util::AtomicFile;

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

/// Message type filter options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MessageTypeFilter {
    /// Show all messages.
    #[default]
    All,
    /// Show only user messages.
    User,
    /// Show only assistant messages.
    Assistant,
    /// Show only system messages.
    System,
    /// Show only tool use/results.
    Tools,
}

impl MessageTypeFilter {
    /// Get display name for the filter.
    #[must_use]
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::All => "All",
            Self::User => "User",
            Self::Assistant => "Assistant",
            Self::System => "System",
            Self::Tools => "Tools",
        }
    }

    /// Cycle to next filter.
    pub fn next(&mut self) {
        *self = match self {
            Self::All => Self::User,
            Self::User => Self::Assistant,
            Self::Assistant => Self::System,
            Self::System => Self::Tools,
            Self::Tools => Self::All,
        };
    }

    /// Cycle to previous filter.
    pub fn prev(&mut self) {
        *self = match self {
            Self::All => Self::Tools,
            Self::User => Self::All,
            Self::Assistant => Self::User,
            Self::System => Self::Assistant,
            Self::Tools => Self::System,
        };
    }
}

/// Filter state for conversation display.
#[derive(Debug, Clone, Default)]
pub struct FilterState {
    /// Whether filter panel is active.
    pub active: bool,
    /// Message type filter.
    pub message_type: MessageTypeFilter,
    /// Date range filter - start date (ISO format).
    pub date_from: Option<chrono::NaiveDate>,
    /// Date range filter - end date (ISO format).
    pub date_to: Option<chrono::NaiveDate>,
    /// Only show messages with errors.
    pub errors_only: bool,
    /// Only show messages with thinking blocks.
    pub thinking_only: bool,
    /// Only show messages with tool use.
    pub tools_only: bool,
    /// Current input mode (for TUI).
    pub input_mode: InputMode,
    /// Buffer for text input (dates, model names).
    pub input_buffer: String,
    /// Model filter (e.g., "sonnet", "opus", "claude-3-5-sonnet").
    pub model_filter: Option<String>,
}

/// Input mode for filter UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputMode {
    /// Not entering any value.
    #[default]
    None,
    /// Entering start date.
    DateFrom,
    /// Entering end date.
    DateTo,
    /// Entering model filter.
    Model,
    /// Entering line number for navigation.
    LineNumber,
}

impl FilterState {
    /// Check if any filters are active.
    #[must_use]
    pub fn is_filtering(&self) -> bool {
        self.message_type != MessageTypeFilter::All
            || self.date_from.is_some()
            || self.date_to.is_some()
            || self.errors_only
            || self.thinking_only
            || self.tools_only
            || self.model_filter.is_some()
    }

    /// Clear all filters.
    pub fn clear(&mut self) {
        self.message_type = MessageTypeFilter::All;
        self.date_from = None;
        self.date_to = None;
        self.errors_only = false;
        self.thinking_only = false;
        self.tools_only = false;
        self.input_mode = InputMode::None;
        self.input_buffer.clear();
        self.model_filter = None;
    }

    /// Check if currently entering input.
    #[must_use]
    pub fn is_entering_input(&self) -> bool {
        self.input_mode != InputMode::None
    }

    /// Check if currently entering a date.
    #[must_use]
    pub fn is_entering_date(&self) -> bool {
        matches!(self.input_mode, InputMode::DateFrom | InputMode::DateTo)
    }

    /// Check if currently entering a model filter.
    #[must_use]
    pub fn is_entering_model(&self) -> bool {
        self.input_mode == InputMode::Model
    }

    /// Start entering a date.
    pub fn start_date_input(&mut self, mode: InputMode) {
        self.input_mode = mode;
        self.input_buffer.clear();
    }

    /// Start entering model filter.
    pub fn start_model_input(&mut self) {
        self.input_mode = InputMode::Model;
        // Pre-fill with current filter if set
        self.input_buffer = self.model_filter.clone().unwrap_or_default();
    }

    /// Cancel input.
    pub fn cancel_input(&mut self) {
        self.input_mode = InputMode::None;
        self.input_buffer.clear();
    }

    /// Add character to input buffer.
    pub fn push_input_char(&mut self, c: char) {
        match self.input_mode {
            InputMode::DateFrom | InputMode::DateTo => {
                // Only allow valid date characters
                if c.is_ascii_digit() || c == '-' {
                    self.input_buffer.push(c);
                }
            }
            InputMode::Model => {
                // Allow alphanumeric and common model name characters
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    self.input_buffer.push(c);
                }
            }
            InputMode::LineNumber => {
                // Only allow digits
                if c.is_ascii_digit() {
                    self.input_buffer.push(c);
                }
            }
            InputMode::None => {}
        }
    }

    /// Remove character from input buffer.
    pub fn pop_input_char(&mut self) {
        self.input_buffer.pop();
    }

    /// Confirm input and apply filter.
    pub fn confirm_input(&mut self) -> bool {
        match self.input_mode {
            InputMode::DateFrom | InputMode::DateTo => self.confirm_date_input(),
            InputMode::Model => self.confirm_model_input(),
            // LineNumber is handled by AppState.confirm_goto_line()
            InputMode::LineNumber | InputMode::None => false,
        }
    }

    /// Confirm date input and apply filter.
    fn confirm_date_input(&mut self) -> bool {
        use chrono::NaiveDate;

        // Try to parse the date
        if let Ok(date) = NaiveDate::parse_from_str(&self.input_buffer, "%Y-%m-%d") {
            match self.input_mode {
                InputMode::DateFrom => self.date_from = Some(date),
                InputMode::DateTo => self.date_to = Some(date),
                _ => {}
            }
            self.input_mode = InputMode::None;
            self.input_buffer.clear();
            true
        } else {
            false
        }
    }

    /// Confirm model input and apply filter.
    fn confirm_model_input(&mut self) -> bool {
        let model = self.input_buffer.trim().to_string();
        if model.is_empty() {
            self.model_filter = None;
        } else {
            self.model_filter = Some(model);
        }
        self.input_mode = InputMode::None;
        self.input_buffer.clear();
        true
    }

    /// Clear the model filter.
    pub fn clear_model_filter(&mut self) {
        self.model_filter = None;
    }

    /// Check if a timestamp falls within the date range.
    #[must_use]
    pub fn is_in_date_range(&self, timestamp: &chrono::DateTime<chrono::Utc>) -> bool {
        let date = timestamp.date_naive();

        if let Some(from) = self.date_from {
            if date < from {
                return false;
            }
        }

        if let Some(to) = self.date_to {
            if date > to {
                return false;
            }
        }

        true
    }

    /// Get filter summary for status bar.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.message_type != MessageTypeFilter::All {
            parts.push(self.message_type.display_name().to_string());
        }
        if self.date_from.is_some() || self.date_to.is_some() {
            let from = self.date_from.map_or("*".to_string(), |d| d.format("%m/%d").to_string());
            let to = self.date_to.map_or("*".to_string(), |d| d.format("%m/%d").to_string());
            parts.push(format!("{from}-{to}"));
        }
        if self.errors_only {
            parts.push("Errors".to_string());
        }
        if self.thinking_only {
            parts.push("Thinking".to_string());
        }
        if self.tools_only {
            parts.push("Tools".to_string());
        }
        if let Some(model) = &self.model_filter {
            parts.push(format!("Model:{model}"));
        }
        if parts.is_empty() {
            "No filters".to_string()
        } else {
            parts.join("+")
        }
    }

    /// Check if a model name matches the filter.
    ///
    /// The filter is case-insensitive and can match any part of the model string.
    /// For example, "sonnet" matches "claude-3-5-sonnet-20241022".
    #[must_use]
    pub fn model_matches(&self, model: &str) -> bool {
        match &self.model_filter {
            Some(filter) => model.to_lowercase().contains(&filter.to_lowercase()),
            None => true,
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

/// A command available in the command palette.
#[derive(Debug, Clone)]
pub struct PaletteCommand {
    /// Command name (displayed and searchable).
    pub name: &'static str,
    /// Short description.
    pub description: &'static str,
    /// Keyboard shortcut hint.
    pub shortcut: &'static str,
    /// Unique command ID.
    pub id: CommandId,
}

/// Command identifiers for command palette actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandId {
    // Navigation
    FocusTree,
    FocusConversation,
    FocusDetails,
    ScrollToTop,
    ScrollToBottom,
    GoToLine,
    // Search
    Search,
    SearchNext,
    SearchPrevious,
    // Display
    ToggleThinking,
    ToggleTools,
    ToggleWordWrap,
    ToggleLineNumbers,
    CycleTheme,
    // Filters
    ToggleFilter,
    CycleMessageFilter,
    ToggleErrorsFilter,
    ClearFilters,
    SetDateFrom,
    SetDateTo,
    SetModelFilter,
    // Actions
    Refresh,
    Export,
    CopyMessage,
    CopyCodeBlock,
    OpenInEditor,
    SelectAll,
    ClearSelection,
    // Help
    ShowHelp,
}

/// Command palette state.
#[derive(Debug, Clone)]
pub struct CommandPalette {
    /// Whether the palette is active.
    pub active: bool,
    /// Current search query.
    pub query: String,
    /// All available commands.
    pub commands: Vec<PaletteCommand>,
    /// Filtered command indices (into commands vec).
    pub filtered: Vec<usize>,
    /// Currently selected index in filtered list.
    pub selected: usize,
}

impl Default for CommandPalette {
    fn default() -> Self {
        let commands = vec![
            // Navigation
            PaletteCommand {
                name: "Focus Tree Panel",
                description: "Switch focus to project/session tree",
                shortcut: "1",
                id: CommandId::FocusTree,
            },
            PaletteCommand {
                name: "Focus Conversation Panel",
                description: "Switch focus to conversation view",
                shortcut: "2",
                id: CommandId::FocusConversation,
            },
            PaletteCommand {
                name: "Focus Details Panel",
                description: "Switch focus to details/analytics",
                shortcut: "3",
                id: CommandId::FocusDetails,
            },
            PaletteCommand {
                name: "Scroll to Top",
                description: "Jump to beginning of content",
                shortcut: "Home",
                id: CommandId::ScrollToTop,
            },
            PaletteCommand {
                name: "Scroll to Bottom",
                description: "Jump to end of content",
                shortcut: "End",
                id: CommandId::ScrollToBottom,
            },
            PaletteCommand {
                name: "Go to Line",
                description: "Jump to specific line number",
                shortcut: "Ctrl+G",
                id: CommandId::GoToLine,
            },
            // Search
            PaletteCommand {
                name: "Search",
                description: "Search in conversation",
                shortcut: "/",
                id: CommandId::Search,
            },
            PaletteCommand {
                name: "Find Next",
                description: "Go to next search result",
                shortcut: "n",
                id: CommandId::SearchNext,
            },
            PaletteCommand {
                name: "Find Previous",
                description: "Go to previous search result",
                shortcut: "N",
                id: CommandId::SearchPrevious,
            },
            // Display
            PaletteCommand {
                name: "Toggle Thinking Blocks",
                description: "Show/hide AI thinking sections",
                shortcut: "t",
                id: CommandId::ToggleThinking,
            },
            PaletteCommand {
                name: "Toggle Tool Outputs",
                description: "Show/hide tool use and results",
                shortcut: "o",
                id: CommandId::ToggleTools,
            },
            PaletteCommand {
                name: "Toggle Word Wrap",
                description: "Enable/disable text wrapping",
                shortcut: "w",
                id: CommandId::ToggleWordWrap,
            },
            PaletteCommand {
                name: "Toggle Line Numbers",
                description: "Show/hide line numbers",
                shortcut: "#",
                id: CommandId::ToggleLineNumbers,
            },
            PaletteCommand {
                name: "Cycle Theme",
                description: "Switch to next color theme",
                shortcut: "T",
                id: CommandId::CycleTheme,
            },
            // Filters
            PaletteCommand {
                name: "Toggle Filter Panel",
                description: "Show/hide filter options",
                shortcut: "f",
                id: CommandId::ToggleFilter,
            },
            PaletteCommand {
                name: "Cycle Message Filter",
                description: "Filter by message type (All/Human/AI)",
                shortcut: "F",
                id: CommandId::CycleMessageFilter,
            },
            PaletteCommand {
                name: "Toggle Errors Only",
                description: "Show only error messages",
                shortcut: "E",
                id: CommandId::ToggleErrorsFilter,
            },
            PaletteCommand {
                name: "Clear All Filters",
                description: "Reset all active filters",
                shortcut: "X",
                id: CommandId::ClearFilters,
            },
            PaletteCommand {
                name: "Set Date From",
                description: "Filter sessions from date",
                shortcut: "[",
                id: CommandId::SetDateFrom,
            },
            PaletteCommand {
                name: "Set Date To",
                description: "Filter sessions until date",
                shortcut: "]",
                id: CommandId::SetDateTo,
            },
            PaletteCommand {
                name: "Set Model Filter",
                description: "Filter by AI model name",
                shortcut: "M",
                id: CommandId::SetModelFilter,
            },
            // Actions
            PaletteCommand {
                name: "Refresh",
                description: "Reload data from disk",
                shortcut: "r",
                id: CommandId::Refresh,
            },
            PaletteCommand {
                name: "Export",
                description: "Export current session",
                shortcut: "e",
                id: CommandId::Export,
            },
            PaletteCommand {
                name: "Copy Message",
                description: "Copy current message to clipboard",
                shortcut: "c",
                id: CommandId::CopyMessage,
            },
            PaletteCommand {
                name: "Copy Code Block",
                description: "Copy code block to clipboard",
                shortcut: "C",
                id: CommandId::CopyCodeBlock,
            },
            PaletteCommand {
                name: "Open in Editor",
                description: "Open conversation in external editor",
                shortcut: "O",
                id: CommandId::OpenInEditor,
            },
            PaletteCommand {
                name: "Select All Sessions",
                description: "Select all sessions in current project",
                shortcut: "Ctrl+A",
                id: CommandId::SelectAll,
            },
            PaletteCommand {
                name: "Clear Selection",
                description: "Deselect all sessions",
                shortcut: "Esc",
                id: CommandId::ClearSelection,
            },
            // Help
            PaletteCommand {
                name: "Show Help",
                description: "Display keyboard shortcuts",
                shortcut: "?",
                id: CommandId::ShowHelp,
            },
        ];

        let filtered: Vec<usize> = (0..commands.len()).collect();

        Self {
            active: false,
            query: String::new(),
            commands,
            filtered,
            selected: 0,
        }
    }
}

impl CommandPalette {
    /// Open the command palette.
    pub fn open(&mut self) {
        self.active = true;
        self.query.clear();
        self.filtered = (0..self.commands.len()).collect();
        self.selected = 0;
    }

    /// Close the command palette.
    pub fn close(&mut self) {
        self.active = false;
        self.query.clear();
    }

    /// Add a character to the search query.
    pub fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.update_filter();
    }

    /// Remove the last character from the query.
    pub fn backspace(&mut self) {
        self.query.pop();
        self.update_filter();
    }

    /// Update filtered commands based on query.
    fn update_filter(&mut self) {
        let query_lower = self.query.to_lowercase();
        self.filtered = self
            .commands
            .iter()
            .enumerate()
            .filter(|(_, cmd)| {
                if query_lower.is_empty() {
                    return true;
                }
                cmd.name.to_lowercase().contains(&query_lower)
                    || cmd.description.to_lowercase().contains(&query_lower)
            })
            .map(|(i, _)| i)
            .collect();

        // Ensure selected is valid
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }

    /// Move selection up.
    pub fn select_prev(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = if self.selected == 0 {
                self.filtered.len() - 1
            } else {
                self.selected - 1
            };
        }
    }

    /// Move selection down.
    pub fn select_next(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = (self.selected + 1) % self.filtered.len();
        }
    }

    /// Get the currently selected command.
    pub fn selected_command(&self) -> Option<&PaletteCommand> {
        self.filtered
            .get(self.selected)
            .and_then(|&idx| self.commands.get(idx))
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
    /// Show line numbers in conversation panel.
    pub show_line_numbers: bool,
    /// Current theme.
    pub theme: Theme,
    /// Current entries (for navigation).
    entries: Vec<LogEntry>,
    /// Search mode state.
    pub search_state: SearchState,
    /// Export dialog state.
    pub export_dialog: ExportDialogState,
    /// Filter state for conversation.
    pub filter_state: FilterState,
    /// Syntax highlighter for code blocks.
    highlighter: SyntaxHighlighter,
    /// Status message to display.
    pub status_message: Option<String>,
    /// Mapping from tree item index to session ID (for session view).
    tree_session_ids: Vec<String>,
    /// Set of selected session IDs (for multi-select export).
    pub selected_sessions: HashSet<String>,
    /// Command palette state.
    pub command_palette: CommandPalette,
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
            show_line_numbers: false,
            theme,
            entries: Vec::new(),
            search_state: SearchState::default(),
            export_dialog: ExportDialogState::default(),
            filter_state: FilterState::default(),
            highlighter: SyntaxHighlighter::new(),
            status_message: None,
            tree_session_ids: Vec::new(),
            selected_sessions: HashSet::new(),
            command_palette: CommandPalette::default(),
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

    /// Toggle selection of the current session (for multi-select).
    ///
    /// Returns true if selection was toggled, false if no session is selected.
    pub fn toggle_session_selection(&mut self) -> bool {
        if let Some(selected) = self.tree_selected {
            if let Some(session_id) = self.tree_session_ids.get(selected).cloned() {
                if self.selected_sessions.contains(&session_id) {
                    self.selected_sessions.remove(&session_id);
                } else {
                    self.selected_sessions.insert(session_id);
                }
                return true;
            }
        }
        false
    }

    /// Check if a session is selected (for multi-select).
    #[must_use]
    pub fn is_session_selected(&self, session_id: &str) -> bool {
        self.selected_sessions.contains(session_id)
    }

    /// Check if the currently highlighted tree item is selected.
    #[must_use]
    pub fn is_current_selected(&self) -> bool {
        if let Some(selected) = self.tree_selected {
            if let Some(session_id) = self.tree_session_ids.get(selected) {
                return self.selected_sessions.contains(session_id);
            }
        }
        false
    }

    /// Clear all selected sessions.
    pub fn clear_selection(&mut self) {
        self.selected_sessions.clear();
    }

    /// Select all sessions in the current view.
    pub fn select_all_sessions(&mut self) {
        for session_id in &self.tree_session_ids {
            self.selected_sessions.insert(session_id.clone());
        }
    }

    /// Get the count of selected sessions.
    #[must_use]
    pub fn selected_session_count(&self) -> usize {
        self.selected_sessions.len()
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

    /// Scroll to a specific line number.
    pub fn scroll_to_line(&mut self, line: usize) {
        if self.conversation_lines.is_empty() {
            return;
        }
        // Line numbers are 1-based for users, but 0-based internally
        let target = line.saturating_sub(1);
        let max = self.conversation_lines.len().saturating_sub(1);
        self.scroll_offset = target.min(max);
        self.status_message = Some(format!("Jumped to line {}", line));
    }

    /// Start go-to-line input mode.
    pub fn start_goto_line(&mut self) {
        self.filter_state.input_mode = InputMode::LineNumber;
        self.filter_state.input_buffer.clear();
    }

    /// Cancel go-to-line input.
    pub fn cancel_goto_line(&mut self) {
        self.filter_state.input_mode = InputMode::None;
        self.filter_state.input_buffer.clear();
    }

    /// Handle character input during go-to-line.
    pub fn goto_line_input(&mut self, c: char) {
        // Only accept digits
        if c.is_ascii_digit() {
            self.filter_state.input_buffer.push(c);
        }
    }

    /// Handle backspace during go-to-line.
    pub fn goto_line_backspace(&mut self) {
        self.filter_state.input_buffer.pop();
    }

    /// Confirm go-to-line and jump.
    pub fn confirm_goto_line(&mut self) {
        if let Ok(line) = self.filter_state.input_buffer.parse::<usize>() {
            if line > 0 {
                self.scroll_to_line(line);
            } else {
                self.status_message = Some("Invalid line number".to_string());
            }
        }
        self.filter_state.input_mode = InputMode::None;
        self.filter_state.input_buffer.clear();
    }

    /// Check if in go-to-line input mode.
    #[must_use]
    pub fn is_entering_line_number(&self) -> bool {
        self.filter_state.input_mode == InputMode::LineNumber
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

    /// Check if command palette is active.
    #[must_use]
    pub fn is_command_palette_active(&self) -> bool {
        self.command_palette.active
    }

    /// Open the command palette.
    pub fn open_command_palette(&mut self) {
        self.command_palette.open();
    }

    /// Close the command palette.
    pub fn close_command_palette(&mut self) {
        self.command_palette.close();
    }

    /// Execute the currently selected command in the palette.
    pub fn execute_selected_command(&mut self) -> Result<Option<CommandId>> {
        let cmd_id = self.command_palette.selected_command().map(|c| c.id);
        self.command_palette.close();

        if let Some(id) = cmd_id {
            self.execute_command(id)?;
        }
        Ok(cmd_id)
    }

    /// Execute a command by ID.
    pub fn execute_command(&mut self, id: CommandId) -> Result<()> {
        match id {
            // Navigation
            CommandId::FocusTree => self.set_focus(0),
            CommandId::FocusConversation => self.set_focus(1),
            CommandId::FocusDetails => self.set_focus(2),
            CommandId::ScrollToTop => self.scroll_to_top(),
            CommandId::ScrollToBottom => self.scroll_to_bottom(),
            CommandId::GoToLine => self.start_goto_line(),
            // Search
            CommandId::Search => self.start_search(),
            CommandId::SearchNext => self.search_next(),
            CommandId::SearchPrevious => self.search_prev(),
            // Display
            CommandId::ToggleThinking => self.toggle_thinking(),
            CommandId::ToggleTools => self.toggle_tools(),
            CommandId::ToggleWordWrap => self.toggle_word_wrap(),
            CommandId::ToggleLineNumbers => self.toggle_line_numbers(),
            CommandId::CycleTheme => self.cycle_theme(),
            // Filters
            CommandId::ToggleFilter => self.toggle_filter(),
            CommandId::CycleMessageFilter => self.cycle_message_filter(),
            CommandId::ToggleErrorsFilter => self.toggle_errors_filter(),
            CommandId::ClearFilters => self.clear_filters(),
            CommandId::SetDateFrom => self.start_date_from_input(),
            CommandId::SetDateTo => self.start_date_to_input(),
            CommandId::SetModelFilter => self.toggle_model_filter(),
            // Actions
            CommandId::Refresh => {
                self.refresh()?;
            }
            CommandId::Export => {
                self.export()?;
            }
            CommandId::CopyMessage => {
                self.copy_message()?;
            }
            CommandId::CopyCodeBlock => {
                self.copy_code_block()?;
            }
            CommandId::OpenInEditor => {
                self.open_in_editor()?;
            }
            CommandId::SelectAll => self.select_all_sessions(),
            CommandId::ClearSelection => self.clear_selection(),
            // Help
            CommandId::ShowHelp => self.toggle_help(),
        }
        Ok(())
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
        // If there are selected sessions (multi-select), export all of them
        if !self.selected_sessions.is_empty() {
            return self.export_selected_sessions();
        }

        // Otherwise, fall back to exporting the current session
        let Some(session_id) = &self.current_session.clone() else {
            self.export_dialog.status_message = Some("No session selected".to_string());
            return Ok(());
        };

        self.export_single_session(session_id)
    }

    /// Export all selected sessions.
    fn export_selected_sessions(&mut self) -> Result<()> {
        let session_ids: Vec<String> = self.selected_sessions.iter().cloned().collect();
        let count = session_ids.len();

        self.export_dialog.exporting = true;

        let format = self.export_dialog.selected_format();
        let extension = format.extension();
        let options = ExportOptions::default()
            .with_thinking(self.export_dialog.include_thinking)
            .with_tool_use(self.export_dialog.include_tools);

        let output_dir = std::env::current_dir().unwrap_or_default();
        let mut exported = 0;
        let mut errors = Vec::new();

        for session_id in &session_ids {
            let Some(session) = self.claude_dir.find_session(session_id)? else {
                errors.push(format!("Session not found: {}", &session_id[..8.min(session_id.len())]));
                continue;
            };

            // Parse the session
            let mut parser = JsonlParser::new();
            let entries = match parser.parse_file(session.path()) {
                Ok(e) => e,
                Err(e) => {
                    errors.push(format!("Parse error: {e}"));
                    continue;
                }
            };

            let conversation = match Conversation::from_entries(entries) {
                Ok(c) => c,
                Err(e) => {
                    errors.push(format!("Reconstruction error: {e}"));
                    continue;
                }
            };

            // Determine output path
            let output_path = output_dir
                .join(format!("session_{}.{}", &session_id[..8.min(session_id.len())], extension));

            // Export the session
            match self.export_conversation_to_file(&conversation, &output_path, &format, &options) {
                Ok(_) => exported += 1,
                Err(e) => errors.push(format!("Export error: {e}")),
            }
        }

        self.export_dialog.exporting = false;

        // Clear selection after export
        self.selected_sessions.clear();

        // Build status message
        if errors.is_empty() {
            self.export_dialog.status_message = Some(format!(
                "Exported {} session{} to current directory",
                exported,
                if exported == 1 { "" } else { "s" }
            ));
        } else {
            self.export_dialog.status_message = Some(format!(
                "Exported {}/{} sessions. Errors: {}",
                exported,
                count,
                errors.len()
            ));
        }

        Ok(())
    }

    /// Export a single session by ID.
    fn export_single_session(&mut self, session_id: &str) -> Result<()> {
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

        self.export_conversation_to_file(&conversation, &output_path, &format, &options)?;

        self.export_dialog.exporting = false;
        self.export_dialog.status_message = Some(format!("Exported to: {}", output_path.display()));

        Ok(())
    }

    /// Export a conversation to a file.
    fn export_conversation_to_file(
        &self,
        conversation: &Conversation,
        output_path: &std::path::Path,
        format: &ExportFormat,
        options: &ExportOptions,
    ) -> Result<()> {
        // Handle SQLite separately as it manages its own file
        if matches!(format, ExportFormat::Sqlite) {
            let exporter = SqliteExporter::new();
            exporter.export_to_file(conversation, output_path, options)?;
            return Ok(());
        }

        // Use atomic file writing for other formats
        let mut atomic = AtomicFile::create(output_path)?;
        let mut writer = BufWriter::new(atomic.writer());

        // Export based on format
        match format {
            ExportFormat::Markdown => {
                let exporter = MarkdownExporter::new();
                exporter.export_conversation(conversation, &mut writer, options)?;
            }
            ExportFormat::Json | ExportFormat::JsonPretty => {
                let exporter = JsonExporter::new()
                    .pretty(matches!(format, ExportFormat::JsonPretty));
                exporter.export_conversation(conversation, &mut writer, options)?;
            }
            ExportFormat::Html => {
                let exporter = HtmlExporter::new()
                    .dark_theme(true);
                exporter.export_conversation(conversation, &mut writer, options)?;
            }
            ExportFormat::Text => {
                let exporter = TextExporter::new();
                exporter.export_conversation(conversation, &mut writer, options)?;
            }
            ExportFormat::Csv => {
                let exporter = CsvExporter::new();
                exporter.export_conversation(conversation, &mut writer, options)?;
            }
            ExportFormat::Xml => {
                let exporter = XmlExporter::new();
                exporter.export_conversation(conversation, &mut writer, options)?;
            }
            ExportFormat::Sqlite => {
                unreachable!("SQLite handled above");
            }
        }

        // Flush BufWriter before finishing atomic write
        use std::io::Write;
        writer.flush()?;
        drop(writer);

        // Complete the atomic write
        atomic.finish()?;

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

    /// Toggle line numbers display.
    pub fn toggle_line_numbers(&mut self) {
        self.show_line_numbers = !self.show_line_numbers;
        if self.show_line_numbers {
            self.status_message = Some("Line numbers: ON".to_string());
        } else {
            self.status_message = Some("Line numbers: OFF".to_string());
        }
        self.update_conversation_display();
    }

    /// Toggle filter panel.
    pub fn toggle_filter(&mut self) {
        self.filter_state.active = !self.filter_state.active;
        if self.filter_state.active {
            self.status_message = Some(format!("Filter: {}", self.filter_state.summary()));
        } else {
            self.status_message = Some("Filter panel closed".to_string());
        }
    }

    /// Cycle through message type filters.
    pub fn cycle_message_filter(&mut self) {
        self.filter_state.message_type.next();
        self.status_message = Some(format!("Filter: {}", self.filter_state.message_type.display_name()));
        self.update_conversation_display();
    }

    /// Toggle errors-only filter.
    pub fn toggle_errors_filter(&mut self) {
        self.filter_state.errors_only = !self.filter_state.errors_only;
        self.status_message = Some(format!(
            "Errors only: {}",
            if self.filter_state.errors_only { "ON" } else { "OFF" }
        ));
        self.update_conversation_display();
    }

    /// Clear all filters.
    pub fn clear_filters(&mut self) {
        self.filter_state.clear();
        self.status_message = Some("Filters cleared".to_string());
        self.update_conversation_display();
    }

    /// Start entering date-from filter.
    pub fn start_date_from_input(&mut self) {
        self.filter_state.start_date_input(InputMode::DateFrom);
        self.status_message = Some("Enter start date (YYYY-MM-DD):".to_string());
    }

    /// Start entering date-to filter.
    pub fn start_date_to_input(&mut self) {
        self.filter_state.start_date_input(InputMode::DateTo);
        self.status_message = Some("Enter end date (YYYY-MM-DD):".to_string());
    }

    /// Start entering model filter.
    pub fn start_model_input(&mut self) {
        self.filter_state.start_model_input();
        self.status_message = Some("Enter model filter (e.g., 'sonnet', 'opus'):".to_string());
    }

    /// Handle character input during filter entry.
    pub fn filter_input(&mut self, c: char) {
        self.filter_state.push_input_char(c);
    }

    /// Handle backspace during filter entry.
    pub fn filter_backspace(&mut self) {
        self.filter_state.pop_input_char();
    }

    /// Confirm filter input.
    pub fn confirm_filter_input(&mut self) {
        let mode = self.filter_state.input_mode;

        // Handle line number input separately
        if mode == InputMode::LineNumber {
            self.confirm_goto_line();
            return;
        }

        if self.filter_state.confirm_input() {
            self.status_message = Some(format!("Filter: {}", self.filter_state.summary()));
            self.update_conversation_display();
        } else if matches!(mode, InputMode::DateFrom | InputMode::DateTo) {
            self.status_message = Some("Invalid date format. Use YYYY-MM-DD".to_string());
        }
    }

    /// Cancel filter input.
    pub fn cancel_filter_input(&mut self) {
        self.filter_state.cancel_input();
        self.status_message = Some("Input cancelled".to_string());
    }

    /// Check if currently entering any filter input.
    #[must_use]
    pub fn is_entering_input(&self) -> bool {
        self.filter_state.is_entering_input()
    }

    /// Check if currently entering a date.
    #[must_use]
    pub fn is_entering_date(&self) -> bool {
        self.filter_state.is_entering_date()
    }

    /// Check if currently entering a model filter.
    #[must_use]
    pub fn is_entering_model(&self) -> bool {
        self.filter_state.is_entering_model()
    }

    /// Get the current input buffer for display.
    #[must_use]
    pub fn input_buffer(&self) -> &str {
        &self.filter_state.input_buffer
    }

    /// Get the current input mode.
    #[must_use]
    pub fn input_mode(&self) -> InputMode {
        self.filter_state.input_mode
    }

    /// Clear the model filter.
    pub fn clear_model_filter(&mut self) {
        self.filter_state.clear_model_filter();
        self.status_message = Some("Model filter cleared".to_string());
        self.update_conversation_display();
    }

    /// Toggle the model filter (enable input if not active, clear if active).
    pub fn toggle_model_filter(&mut self) {
        if self.filter_state.model_filter.is_some() {
            self.clear_model_filter();
        } else {
            self.start_model_input();
        }
    }

    /// Check if filter panel is active.
    #[must_use]
    pub fn is_filter_active(&self) -> bool {
        self.filter_state.active
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

    /// Open current conversation in external editor.
    pub fn open_in_editor(&mut self) -> Result<()> {
        if self.current_session.is_none() {
            self.status_message = Some("No session selected".to_string());
            return Ok(());
        }

        // Get the editor from environment or use sensible defaults
        let editor = std::env::var("EDITOR")
            .or_else(|_| std::env::var("VISUAL"))
            .unwrap_or_else(|_| {
                // Platform-specific defaults
                if cfg!(windows) {
                    "notepad".to_string()
                } else {
                    "vi".to_string()
                }
            });

        // Create a temporary file with the conversation content
        let content = self.get_current_message_text();
        if content.is_empty() {
            self.status_message = Some("No content to edit".to_string());
            return Ok(());
        }

        // Create temp file with meaningful name
        let session_id = self.current_session.as_ref().unwrap();
        let short_id = if session_id.len() > 8 {
            &session_id[..8]
        } else {
            session_id
        };

        let temp_path = std::env::temp_dir().join(format!("snatch-{}.md", short_id));

        // Write content to temp file
        std::fs::write(&temp_path, &content).map_err(|e| {
            crate::error::SnatchError::io("Failed to create temporary file", e)
        })?;

        // Open editor
        let status = std::process::Command::new(&editor)
            .arg(&temp_path)
            .status();

        match status {
            Ok(exit_status) => {
                if exit_status.success() {
                    self.status_message = Some(format!("Editor closed ({})", editor));
                } else {
                    self.status_message = Some(format!("Editor exited with code: {:?}", exit_status.code()));
                }
            }
            Err(e) => {
                self.status_message = Some(format!("Failed to open editor '{}': {}", editor, e));
            }
        }

        // Clean up temp file (optional, it's in temp dir anyway)
        let _ = std::fs::remove_file(&temp_path);

        Ok(())
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

    /// Check if an entry should be shown based on current filters.
    fn should_show_entry(&self, entry: &LogEntry) -> bool {
        // Check message type filter
        match (&self.filter_state.message_type, entry) {
            (MessageTypeFilter::All, _) => {}
            (MessageTypeFilter::User, LogEntry::User(_)) => {}
            (MessageTypeFilter::Assistant, LogEntry::Assistant(_)) => {}
            (MessageTypeFilter::System, LogEntry::System(_)) => {}
            (MessageTypeFilter::Tools, LogEntry::Assistant(a)) => {
                // Only show if entry has tool use/result blocks
                let has_tools = a.message.content.iter().any(|b| {
                    matches!(b, ContentBlock::ToolUse(_) | ContentBlock::ToolResult(_))
                });
                if !has_tools {
                    return false;
                }
            }
            _ => return false,
        }

        // Check errors-only filter
        if self.filter_state.errors_only {
            let has_error = match entry {
                LogEntry::Assistant(a) => a.message.content.iter().any(|b| {
                    matches!(b, ContentBlock::ToolResult(r) if r.is_explicit_error())
                }),
                _ => false,
            };
            if !has_error {
                return false;
            }
        }

        // Check thinking-only filter
        if self.filter_state.thinking_only {
            let has_thinking = match entry {
                LogEntry::Assistant(a) => a.message.content.iter().any(|b| {
                    matches!(b, ContentBlock::Thinking(_))
                }),
                _ => false,
            };
            if !has_thinking {
                return false;
            }
        }

        // Check tools-only filter
        if self.filter_state.tools_only {
            let has_tools = match entry {
                LogEntry::Assistant(a) => a.message.content.iter().any(|b| {
                    matches!(b, ContentBlock::ToolUse(_) | ContentBlock::ToolResult(_))
                }),
                _ => false,
            };
            if !has_tools {
                return false;
            }
        }

        // Check date range filter
        if self.filter_state.date_from.is_some() || self.filter_state.date_to.is_some() {
            // Get timestamp if available - entries without timestamps pass through
            let timestamp = match entry {
                LogEntry::User(u) => Some(&u.timestamp),
                LogEntry::Assistant(a) => Some(&a.timestamp),
                LogEntry::System(s) => Some(&s.timestamp),
                LogEntry::QueueOperation(q) => Some(&q.timestamp),
                LogEntry::TurnEnd(t) => Some(&t.timestamp),
                // Summary and FileHistorySnapshot don't have timestamps, include them by default
                LogEntry::Summary(_) | LogEntry::FileHistorySnapshot(_) => None,
            };
            if let Some(ts) = timestamp {
                if !self.filter_state.is_in_date_range(ts) {
                    return false;
                }
            }
        }

        // Check model filter
        if self.filter_state.model_filter.is_some() {
            match entry {
                LogEntry::Assistant(a) => {
                    if !self.filter_state.model_matches(&a.message.model) {
                        return false;
                    }
                }
                // Non-assistant entries don't have a model, exclude them when filtering by model
                _ => return false,
            }
        }

        true
    }

    /// Update conversation display based on current settings.
    fn update_conversation_display(&mut self) {
        self.conversation_lines.clear();

        for entry in &self.entries {
            // Apply message type filter
            if !self.should_show_entry(entry) {
                continue;
            }

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

#[cfg(test)]
mod tests {
    use super::*;

    mod search_state {
        use super::*;

        #[test]
        fn test_default_search_state() {
            let state = SearchState::default();
            assert!(!state.active);
            assert!(state.query.is_empty());
            assert_eq!(state.cursor, 0);
            assert!(state.results.is_empty());
            assert_eq!(state.current_result, 0);
            // case_insensitive defaults to false via Default derive
            assert!(!state.case_insensitive);
        }

        #[test]
        fn test_push_char() {
            let mut state = SearchState::default();
            state.push_char('h');
            state.push_char('e');
            state.push_char('l');
            state.push_char('l');
            state.push_char('o');
            assert_eq!(state.query, "hello");
            assert_eq!(state.cursor, 5);
        }

        #[test]
        fn test_backspace() {
            let mut state = SearchState::default();
            state.query = "hello".to_string();
            state.cursor = 5;

            state.backspace();
            assert_eq!(state.query, "hell");
            assert_eq!(state.cursor, 4);

            state.backspace();
            assert_eq!(state.query, "hel");
            assert_eq!(state.cursor, 3);
        }

        #[test]
        fn test_backspace_at_start() {
            let mut state = SearchState::default();
            state.query = "hello".to_string();
            state.cursor = 0;

            state.backspace();
            assert_eq!(state.query, "hello");
            assert_eq!(state.cursor, 0);
        }

        #[test]
        fn test_next_result() {
            let mut state = SearchState::default();
            state.results = vec![10, 20, 30];
            state.current_result = 0;

            state.next_result();
            assert_eq!(state.current_result, 1);

            state.next_result();
            assert_eq!(state.current_result, 2);

            state.next_result();
            assert_eq!(state.current_result, 0); // wraps around
        }

        #[test]
        fn test_prev_result() {
            let mut state = SearchState::default();
            state.results = vec![10, 20, 30];
            state.current_result = 2;

            state.prev_result();
            assert_eq!(state.current_result, 1);

            state.prev_result();
            assert_eq!(state.current_result, 0);

            state.prev_result();
            assert_eq!(state.current_result, 2); // wraps around
        }

        #[test]
        fn test_next_result_empty() {
            let mut state = SearchState::default();
            state.next_result();
            assert_eq!(state.current_result, 0);
        }

        #[test]
        fn test_current_line() {
            let mut state = SearchState::default();
            assert_eq!(state.current_line(), None);

            state.results = vec![10, 20, 30];
            state.current_result = 1;
            assert_eq!(state.current_line(), Some(20));
        }

        #[test]
        fn test_result_count_str() {
            let mut state = SearchState::default();
            assert_eq!(state.result_count_str(), "No matches");

            state.results = vec![10, 20, 30];
            state.current_result = 1;
            assert_eq!(state.result_count_str(), "2/3");
        }

        #[test]
        fn test_clear() {
            let mut state = SearchState {
                active: true,
                query: "test".to_string(),
                cursor: 4,
                results: vec![1, 2, 3],
                current_result: 2,
                case_insensitive: false,
            };

            state.clear();
            assert!(!state.active);
            assert!(state.query.is_empty());
            assert_eq!(state.cursor, 0);
            assert!(state.results.is_empty());
            assert_eq!(state.current_result, 0);
        }
    }

    mod message_type_filter {
        use super::*;

        #[test]
        fn test_default_filter() {
            let filter = MessageTypeFilter::default();
            assert_eq!(filter, MessageTypeFilter::All);
        }

        #[test]
        fn test_display_name() {
            assert_eq!(MessageTypeFilter::All.display_name(), "All");
            assert_eq!(MessageTypeFilter::User.display_name(), "User");
            assert_eq!(MessageTypeFilter::Assistant.display_name(), "Assistant");
            assert_eq!(MessageTypeFilter::System.display_name(), "System");
            assert_eq!(MessageTypeFilter::Tools.display_name(), "Tools");
        }

        #[test]
        fn test_next_cycle() {
            let mut filter = MessageTypeFilter::All;

            filter.next();
            assert_eq!(filter, MessageTypeFilter::User);

            filter.next();
            assert_eq!(filter, MessageTypeFilter::Assistant);

            filter.next();
            assert_eq!(filter, MessageTypeFilter::System);

            filter.next();
            assert_eq!(filter, MessageTypeFilter::Tools);

            filter.next();
            assert_eq!(filter, MessageTypeFilter::All); // wraps around
        }

        #[test]
        fn test_prev_cycle() {
            let mut filter = MessageTypeFilter::All;

            filter.prev();
            assert_eq!(filter, MessageTypeFilter::Tools);

            filter.prev();
            assert_eq!(filter, MessageTypeFilter::System);

            filter.prev();
            assert_eq!(filter, MessageTypeFilter::Assistant);

            filter.prev();
            assert_eq!(filter, MessageTypeFilter::User);

            filter.prev();
            assert_eq!(filter, MessageTypeFilter::All); // wraps around
        }
    }

    mod filter_state {
        use super::*;

        #[test]
        fn test_default_filter_state() {
            let state = FilterState::default();
            assert!(!state.active);
            assert_eq!(state.message_type, MessageTypeFilter::All);
            assert!(state.date_from.is_none());
            assert!(state.date_to.is_none());
            assert!(!state.errors_only);
            assert!(!state.thinking_only);
            assert!(!state.tools_only);
            assert!(state.model_filter.is_none());
            assert!(!state.is_filtering());
        }

        #[test]
        fn test_model_filter_makes_is_filtering_true() {
            let mut state = FilterState::default();
            assert!(!state.is_filtering());

            state.model_filter = Some("sonnet".to_string());
            assert!(state.is_filtering());
        }

        #[test]
        fn test_model_matches_case_insensitive() {
            let mut state = FilterState::default();
            state.model_filter = Some("sonnet".to_string());

            assert!(state.model_matches("claude-3-5-sonnet-20241022"));
            assert!(state.model_matches("claude-3-5-SONNET-20241022"));
            assert!(state.model_matches("Sonnet"));
            assert!(!state.model_matches("claude-opus-4-5-20251101"));
        }

        #[test]
        fn test_model_matches_partial() {
            let mut state = FilterState::default();
            state.model_filter = Some("opus".to_string());

            assert!(state.model_matches("claude-opus-4-5-20251101"));
            assert!(state.model_matches("claude-opus-4-20240229"));
            assert!(!state.model_matches("claude-3-5-sonnet-20241022"));
        }

        #[test]
        fn test_model_matches_no_filter() {
            let state = FilterState::default();
            // When no filter is set, all models match
            assert!(state.model_matches("claude-opus-4-5-20251101"));
            assert!(state.model_matches("claude-3-5-sonnet-20241022"));
            assert!(state.model_matches("anything"));
        }

        #[test]
        fn test_summary_includes_model() {
            let mut state = FilterState::default();
            state.model_filter = Some("sonnet".to_string());

            let summary = state.summary();
            assert!(summary.contains("Model:sonnet"));
        }

        #[test]
        fn test_clear_resets_model_filter() {
            let mut state = FilterState::default();
            state.model_filter = Some("opus".to_string());
            state.errors_only = true;

            state.clear();
            assert!(state.model_filter.is_none());
            assert!(!state.errors_only);
        }

        #[test]
        fn test_model_input_flow() {
            let mut state = FilterState::default();

            // Start model input
            state.start_model_input();
            assert_eq!(state.input_mode, InputMode::Model);
            assert!(state.input_buffer.is_empty());

            // Type "opus"
            state.push_input_char('o');
            state.push_input_char('p');
            state.push_input_char('u');
            state.push_input_char('s');
            assert_eq!(state.input_buffer, "opus");

            // Confirm
            assert!(state.confirm_input());
            assert_eq!(state.model_filter, Some("opus".to_string()));
            assert_eq!(state.input_mode, InputMode::None);
            assert!(state.input_buffer.is_empty());
        }

        #[test]
        fn test_model_input_cancel() {
            let mut state = FilterState::default();

            state.start_model_input();
            state.push_input_char('o');
            state.push_input_char('p');

            state.cancel_input();
            assert_eq!(state.input_mode, InputMode::None);
            assert!(state.input_buffer.is_empty());
            assert!(state.model_filter.is_none());
        }

        #[test]
        fn test_model_input_with_existing_filter() {
            let mut state = FilterState::default();
            state.model_filter = Some("opus".to_string());

            // Start input should pre-fill with existing filter
            state.start_model_input();
            assert_eq!(state.input_buffer, "opus");
        }

        #[test]
        fn test_model_input_empty_clears_filter() {
            let mut state = FilterState::default();
            state.model_filter = Some("opus".to_string());

            state.start_model_input();
            state.input_buffer.clear(); // User deletes all content

            state.confirm_input();
            assert!(state.model_filter.is_none());
        }
    }

    mod input_mode {
        use super::*;

        #[test]
        fn test_default_input_mode() {
            let mode = InputMode::default();
            assert_eq!(mode, InputMode::None);
        }

        #[test]
        fn test_is_entering_input() {
            let mut state = FilterState::default();
            assert!(!state.is_entering_input());

            state.input_mode = InputMode::DateFrom;
            assert!(state.is_entering_input());

            state.input_mode = InputMode::Model;
            assert!(state.is_entering_input());

            state.input_mode = InputMode::None;
            assert!(!state.is_entering_input());
        }

        #[test]
        fn test_is_entering_date() {
            let mut state = FilterState::default();
            assert!(!state.is_entering_date());

            state.input_mode = InputMode::DateFrom;
            assert!(state.is_entering_date());

            state.input_mode = InputMode::DateTo;
            assert!(state.is_entering_date());

            state.input_mode = InputMode::Model;
            assert!(!state.is_entering_date());
        }

        #[test]
        fn test_is_entering_model() {
            let mut state = FilterState::default();
            assert!(!state.is_entering_model());

            state.input_mode = InputMode::Model;
            assert!(state.is_entering_model());

            state.input_mode = InputMode::DateFrom;
            assert!(!state.is_entering_model());
        }

        #[test]
        fn test_line_number_input_mode() {
            let mut state = FilterState::default();
            assert_eq!(state.input_mode, InputMode::None);

            state.input_mode = InputMode::LineNumber;
            assert!(state.is_entering_input());
            assert!(!state.is_entering_date());
            assert!(!state.is_entering_model());

            // Test that only digits are accepted
            state.push_input_char('1');
            state.push_input_char('2');
            state.push_input_char('3');
            assert_eq!(state.input_buffer, "123");

            // Non-digit should be ignored
            state.push_input_char('a');
            state.push_input_char('-');
            assert_eq!(state.input_buffer, "123");

            // Backspace works
            state.pop_input_char();
            assert_eq!(state.input_buffer, "12");
        }
    }

    mod command_palette {
        use super::*;

        #[test]
        fn test_default_command_palette() {
            let palette = CommandPalette::default();
            assert!(!palette.active);
            assert!(palette.query.is_empty());
            assert!(!palette.commands.is_empty());
            assert_eq!(palette.selected, 0);
            // All commands should be in filtered initially
            assert_eq!(palette.filtered.len(), palette.commands.len());
        }

        #[test]
        fn test_open_and_close() {
            let mut palette = CommandPalette::default();
            palette.query = "test".to_string();
            palette.selected = 5;

            palette.open();
            assert!(palette.active);
            assert!(palette.query.is_empty());
            assert_eq!(palette.selected, 0);

            palette.close();
            assert!(!palette.active);
        }

        #[test]
        fn test_filter_commands() {
            let mut palette = CommandPalette::default();

            // Type "toggle" to filter
            palette.push_char('t');
            palette.push_char('o');
            palette.push_char('g');
            palette.push_char('g');
            palette.push_char('l');
            palette.push_char('e');

            // Should only show commands with "toggle" in name or description
            assert!(palette.filtered.len() < palette.commands.len());
            for &idx in &palette.filtered {
                let cmd = &palette.commands[idx];
                assert!(
                    cmd.name.to_lowercase().contains("toggle")
                        || cmd.description.to_lowercase().contains("toggle")
                );
            }
        }

        #[test]
        fn test_navigation() {
            let mut palette = CommandPalette::default();
            assert_eq!(palette.selected, 0);

            palette.select_next();
            assert_eq!(palette.selected, 1);

            palette.select_next();
            assert_eq!(palette.selected, 2);

            palette.select_prev();
            assert_eq!(palette.selected, 1);

            palette.select_prev();
            assert_eq!(palette.selected, 0);

            // Should wrap to end
            palette.select_prev();
            assert_eq!(palette.selected, palette.filtered.len() - 1);
        }

        #[test]
        fn test_selected_command() {
            let palette = CommandPalette::default();
            let cmd = palette.selected_command();
            assert!(cmd.is_some());
            assert_eq!(cmd.unwrap().name, palette.commands[0].name);
        }

        #[test]
        fn test_backspace() {
            let mut palette = CommandPalette::default();
            palette.push_char('t');
            palette.push_char('e');
            palette.push_char('s');
            palette.push_char('t');
            assert_eq!(palette.query, "test");

            palette.backspace();
            assert_eq!(palette.query, "tes");

            palette.backspace();
            palette.backspace();
            palette.backspace();
            assert!(palette.query.is_empty());

            // Backspace on empty should be fine
            palette.backspace();
            assert!(palette.query.is_empty());
        }
    }
}
