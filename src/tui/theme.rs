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

/// Theme implementation providing constructors and style accessors.
///
/// Some style methods (e.g., `warning_style`, `success_style`) are not yet
/// used in the TUI but are intentionally provided for completeness and
/// future UI enhancements.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dark_theme() {
        let theme = Theme::dark();
        assert_eq!(theme.name, "dark");
        assert_eq!(theme.primary, Color::Cyan);
        assert_eq!(theme.foreground, Color::White);
    }

    #[test]
    fn test_light_theme() {
        let theme = Theme::light();
        assert_eq!(theme.name, "light");
        assert_eq!(theme.background, Color::White);
        assert_eq!(theme.foreground, Color::Black);
    }

    #[test]
    fn test_high_contrast_theme() {
        let theme = Theme::high_contrast();
        assert_eq!(theme.name, "high-contrast");
        assert_eq!(theme.background, Color::Black);
        assert_eq!(theme.foreground, Color::White);
        assert_eq!(theme.primary, Color::Yellow);
    }

    #[test]
    fn test_default_theme_is_dark() {
        let default = Theme::default();
        let dark = Theme::dark();
        assert_eq!(default.name, dark.name);
        assert_eq!(default.primary, dark.primary);
    }

    #[test]
    fn test_from_name() {
        assert!(Theme::from_name("dark").is_some());
        assert!(Theme::from_name("light").is_some());
        assert!(Theme::from_name("high-contrast").is_some());
        assert!(Theme::from_name("highcontrast").is_some());
        assert!(Theme::from_name("DARK").is_some()); // case insensitive
        assert!(Theme::from_name("unknown").is_none());
    }

    #[test]
    fn test_border_style() {
        let theme = Theme::dark();
        let style = theme.border_style();
        assert_eq!(style.fg, Some(theme.border));
    }

    #[test]
    fn test_border_focused_style() {
        let theme = Theme::dark();
        let style = theme.border_focused_style();
        assert_eq!(style.fg, Some(theme.border_focused));
    }

    #[test]
    fn test_selection_style() {
        let theme = Theme::dark();
        let style = theme.selection_style();
        assert_eq!(style.bg, Some(theme.selection));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn test_user_style() {
        let theme = Theme::dark();
        let style = theme.user_style();
        assert_eq!(style.fg, Some(theme.user));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn test_assistant_style() {
        let theme = Theme::dark();
        let style = theme.assistant_style();
        assert_eq!(style.fg, Some(theme.assistant));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn test_thinking_style() {
        let theme = Theme::dark();
        let style = theme.thinking_style();
        assert_eq!(style.fg, Some(theme.thinking));
        assert!(style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn test_error_style() {
        let theme = Theme::dark();
        let style = theme.error_style();
        assert_eq!(style.fg, Some(theme.error));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn test_available_themes() {
        let themes = available_themes();
        assert_eq!(themes.len(), 3);
        assert!(themes.contains(&"dark"));
        assert!(themes.contains(&"light"));
        assert!(themes.contains(&"high-contrast"));
    }

    #[test]
    fn test_tool_style() {
        let theme = Theme::dark();
        let style = theme.tool_style();
        assert_eq!(style.fg, Some(theme.tool));
    }

    #[test]
    fn test_warning_style() {
        let theme = Theme::dark();
        let style = theme.warning_style();
        assert_eq!(style.fg, Some(theme.warning));
    }

    #[test]
    fn test_success_style() {
        let theme = Theme::dark();
        let style = theme.success_style();
        assert_eq!(style.fg, Some(theme.success));
    }
}
