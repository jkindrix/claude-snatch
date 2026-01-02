//! Reusable TUI components.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

/// A scrollable text view.
pub struct ScrollableText<'a> {
    title: &'a str,
    content: Vec<Line<'a>>,
    scroll: usize,
    focused: bool,
}

impl<'a> ScrollableText<'a> {
    /// Create a new scrollable text view.
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            content: Vec::new(),
            scroll: 0,
            focused: false,
        }
    }

    /// Set content.
    pub fn content(mut self, content: Vec<Line<'a>>) -> Self {
        self.content = content;
        self
    }

    /// Set scroll position.
    pub fn scroll(mut self, scroll: usize) -> Self {
        self.scroll = scroll;
        self
    }

    /// Set focused state.
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Render the component.
    pub fn render(self, f: &mut Frame, area: Rect) {
        let border_style = if self.focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let visible_content: Vec<Line> = self.content
            .into_iter()
            .skip(self.scroll)
            .take(area.height as usize - 2)
            .collect();

        let paragraph = Paragraph::new(visible_content)
            .block(
                Block::default()
                    .title(self.title)
                    .borders(Borders::ALL)
                    .border_style(border_style),
            )
            .wrap(Wrap { trim: false });

        f.render_widget(paragraph, area);
    }
}

/// A status bar component.
pub struct StatusBar<'a> {
    left: Vec<Span<'a>>,
    right: Vec<Span<'a>>,
}

impl<'a> StatusBar<'a> {
    /// Create a new status bar.
    pub fn new() -> Self {
        Self {
            left: Vec::new(),
            right: Vec::new(),
        }
    }

    /// Add left-aligned content.
    pub fn left(mut self, spans: Vec<Span<'a>>) -> Self {
        self.left = spans;
        self
    }

    /// Add right-aligned content.
    pub fn right(mut self, spans: Vec<Span<'a>>) -> Self {
        self.right = spans;
        self
    }

    /// Render the status bar.
    pub fn render(self, f: &mut Frame, area: Rect) {
        // Calculate text lengths for padding (use saturating_sub to prevent underflow)
        let left_len: usize = self.left.iter().map(|s| s.content.chars().count()).sum();
        let right_len: usize = self.right.iter().map(|s| s.content.chars().count()).sum();
        let available_width = area.width as usize;

        // Calculate padding between left and right content
        let padding = available_width
            .saturating_sub(left_len)
            .saturating_sub(right_len)
            .max(1);
        let padding_str = " ".repeat(padding);

        // Build the line preserving original styling
        let mut spans: Vec<Span> = self.left;
        spans.push(Span::raw(padding_str));
        spans.extend(self.right);

        let line = Line::from(spans);
        let paragraph = Paragraph::new(vec![line])
            .style(Style::default().bg(Color::DarkGray).fg(Color::White));

        f.render_widget(paragraph, area);
    }
}

impl<'a> Default for StatusBar<'a> {
    fn default() -> Self {
        Self::new()
    }
}

/// Message type indicator.
#[derive(Debug, Clone, Copy)]
pub enum MessageType {
    User,
    Assistant,
    System,
    Summary,
    Tool,
}

impl MessageType {
    /// Get the display icon.
    pub fn icon(&self) -> &'static str {
        match self {
            Self::User => "👤",
            Self::Assistant => "🤖",
            Self::System => "⚙️",
            Self::Summary => "📋",
            Self::Tool => "🔧",
        }
    }

    /// Get the display color.
    pub fn color(&self) -> Color {
        match self {
            Self::User => Color::Green,
            Self::Assistant => Color::Blue,
            Self::System => Color::Yellow,
            Self::Summary => Color::Magenta,
            Self::Tool => Color::Cyan,
        }
    }
}

/// Format a message header line.
pub fn format_message_header(msg_type: MessageType, timestamp: Option<&str>) -> Line<'static> {
    let mut spans = vec![
        Span::raw(msg_type.icon()),
        Span::raw(" "),
        Span::styled(
            format!("{msg_type:?}"),
            Style::default()
                .fg(msg_type.color())
                .add_modifier(Modifier::BOLD),
        ),
    ];

    if let Some(ts) = timestamp {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            ts.to_string(),
            Style::default().fg(Color::DarkGray),
        ));
    }

    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    mod message_type_tests {
        use super::*;

        #[test]
        fn test_user_icon() {
            assert_eq!(MessageType::User.icon(), "👤");
        }

        #[test]
        fn test_assistant_icon() {
            assert_eq!(MessageType::Assistant.icon(), "🤖");
        }

        #[test]
        fn test_system_icon() {
            assert_eq!(MessageType::System.icon(), "⚙️");
        }

        #[test]
        fn test_summary_icon() {
            assert_eq!(MessageType::Summary.icon(), "📋");
        }

        #[test]
        fn test_tool_icon() {
            assert_eq!(MessageType::Tool.icon(), "🔧");
        }

        #[test]
        fn test_user_color() {
            assert_eq!(MessageType::User.color(), Color::Green);
        }

        #[test]
        fn test_assistant_color() {
            assert_eq!(MessageType::Assistant.color(), Color::Blue);
        }

        #[test]
        fn test_system_color() {
            assert_eq!(MessageType::System.color(), Color::Yellow);
        }

        #[test]
        fn test_summary_color() {
            assert_eq!(MessageType::Summary.color(), Color::Magenta);
        }

        #[test]
        fn test_tool_color() {
            assert_eq!(MessageType::Tool.color(), Color::Cyan);
        }
    }

    mod format_message_header_tests {
        use super::*;

        #[test]
        fn test_format_without_timestamp() {
            let header = format_message_header(MessageType::User, None);
            assert!(!header.spans.is_empty());
            // Should have icon, space, and styled type name
            assert_eq!(header.spans.len(), 3);
        }

        #[test]
        fn test_format_with_timestamp() {
            let header = format_message_header(MessageType::Assistant, Some("2025-01-01 12:00:00"));
            // Should have icon, space, styled type name, space, and timestamp
            assert_eq!(header.spans.len(), 5);
        }

        #[test]
        fn test_all_message_types_format() {
            // Ensure all message types can be formatted without panic
            let types = [
                MessageType::User,
                MessageType::Assistant,
                MessageType::System,
                MessageType::Summary,
                MessageType::Tool,
            ];

            for msg_type in types {
                let _ = format_message_header(msg_type, None);
                let _ = format_message_header(msg_type, Some("timestamp"));
            }
        }
    }

    mod scrollable_text_tests {
        use super::*;

        #[test]
        fn test_scrollable_text_builder() {
            let text = ScrollableText::new("Test")
                .content(vec![Line::from("Hello")])
                .scroll(5)
                .focused(true);

            assert_eq!(text.title, "Test");
            assert_eq!(text.scroll, 5);
            assert!(text.focused);
        }

        #[test]
        fn test_scrollable_text_default_values() {
            let text = ScrollableText::new("Default");

            assert_eq!(text.scroll, 0);
            assert!(!text.focused);
            assert!(text.content.is_empty());
        }
    }

    mod status_bar_tests {
        use super::*;

        #[test]
        fn test_status_bar_default() {
            let bar = StatusBar::default();
            assert!(bar.left.is_empty());
            assert!(bar.right.is_empty());
        }

        #[test]
        fn test_status_bar_builder() {
            let bar = StatusBar::new()
                .left(vec![Span::raw("Left")])
                .right(vec![Span::raw("Right")]);

            assert_eq!(bar.left.len(), 1);
            assert_eq!(bar.right.len(), 1);
        }
    }
}
