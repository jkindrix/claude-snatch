//! TUI theming and colors.

use ratatui::style::{Color, Modifier, Style};

/// Application theme.
///
/// This struct provides a comprehensive theming system for the TUI.
/// Some fields and methods are intentionally reserved for future use
/// (e.g., styled message rendering, error display, etc.).
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Theme {
    /// Name of the theme.
    pub name: String,
    /// Background color.
    pub background: Color,
    /// Foreground color.
    pub foreground: Color,
    /// Primary accent color.
    pub primary: Color,
    /// Secondary accent color.
    pub secondary: Color,
    /// Border color (unfocused).
    pub border: Color,
    /// Border color (focused).
    pub border_focused: Color,
    /// Selection highlight.
    pub selection: Color,
    /// User message color.
    pub user: Color,
    /// Assistant message color.
    pub assistant: Color,
    /// System message color.
    pub system: Color,
    /// Thinking block color.
    pub thinking: Color,
    /// Tool use color.
    pub tool: Color,
    /// Error color.
    pub error: Color,
    /// Warning color.
    pub warning: Color,
    /// Success color.
    pub success: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

#[allow(dead_code)]
impl Theme {
    /// Create the default dark theme.
    pub fn dark() -> Self {
        Self {
            name: "dark".to_string(),
            background: Color::Reset,
            foreground: Color::White,
            primary: Color::Cyan,
            secondary: Color::Magenta,
            border: Color::DarkGray,
            border_focused: Color::Cyan,
            selection: Color::DarkGray,
            user: Color::Green,
            assistant: Color::Blue,
            system: Color::Yellow,
            thinking: Color::Magenta,
            tool: Color::Cyan,
            error: Color::Red,
            warning: Color::Yellow,
            success: Color::Green,
        }
    }

    /// Create a light theme.
    pub fn light() -> Self {
        Self {
            name: "light".to_string(),
            background: Color::White,
            foreground: Color::Black,
            primary: Color::Blue,
            secondary: Color::Magenta,
            border: Color::Gray,
            border_focused: Color::Blue,
            selection: Color::LightBlue,
            user: Color::Green,
            assistant: Color::Blue,
            system: Color::Yellow,
            thinking: Color::Magenta,
            tool: Color::Cyan,
            error: Color::Red,
            warning: Color::Yellow,
            success: Color::Green,
        }
    }

    /// Create a high contrast theme.
    pub fn high_contrast() -> Self {
        Self {
            name: "high-contrast".to_string(),
            background: Color::Black,
            foreground: Color::White,
            primary: Color::Yellow,
            secondary: Color::Cyan,
            border: Color::White,
            border_focused: Color::Yellow,
            selection: Color::White,
            user: Color::Green,
            assistant: Color::Cyan,
            system: Color::Yellow,
            thinking: Color::Magenta,
            tool: Color::Cyan,
            error: Color::Red,
            warning: Color::Yellow,
            success: Color::Green,
        }
    }

    /// Get theme by name.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "dark" => Some(Self::dark()),
            "light" => Some(Self::light()),
            "high-contrast" | "highcontrast" => Some(Self::high_contrast()),
            _ => None,
        }
    }

    /// Get style for borders (unfocused).
    pub fn border_style(&self) -> Style {
        Style::default().fg(self.border)
    }

    /// Get style for focused borders.
    pub fn border_focused_style(&self) -> Style {
        Style::default().fg(self.border_focused)
    }

    /// Get style for selected items.
    pub fn selection_style(&self) -> Style {
        Style::default()
            .bg(self.selection)
            .add_modifier(Modifier::BOLD)
    }

    /// Get style for user messages.
    pub fn user_style(&self) -> Style {
        Style::default()
            .fg(self.user)
            .add_modifier(Modifier::BOLD)
    }

    /// Get style for assistant messages.
    pub fn assistant_style(&self) -> Style {
        Style::default()
            .fg(self.assistant)
            .add_modifier(Modifier::BOLD)
    }

    /// Get style for thinking blocks.
    pub fn thinking_style(&self) -> Style {
        Style::default()
            .fg(self.thinking)
            .add_modifier(Modifier::ITALIC)
    }

    /// Get style for tool use.
    pub fn tool_style(&self) -> Style {
        Style::default().fg(self.tool)
    }

    /// Get style for errors.
    pub fn error_style(&self) -> Style {
        Style::default()
            .fg(self.error)
            .add_modifier(Modifier::BOLD)
    }

    /// Get style for warnings.
    pub fn warning_style(&self) -> Style {
        Style::default().fg(self.warning)
    }

    /// Get style for success.
    pub fn success_style(&self) -> Style {
        Style::default().fg(self.success)
    }
}

/// Available themes list.
pub fn available_themes() -> Vec<&'static str> {
    vec!["dark", "light", "high-contrast"]
}
