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
        // Create status line with left and right content
        let left_text: String = self.left.iter().map(|s| s.content.as_ref()).collect();
        let right_text: String = self.right.iter().map(|s| s.content.as_ref()).collect();

        let padding = area.width as usize - left_text.len() - right_text.len();
        let padding_str = " ".repeat(padding.max(1));

        let line = Line::from(vec![
            Span::raw(left_text),
            Span::raw(padding_str),
            Span::styled(right_text, Style::default().fg(Color::DarkGray)),
        ]);

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
