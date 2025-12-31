//! Syntax highlighting for code blocks in the TUI.
//!
//! Uses syntect to provide syntax highlighting for code blocks
//! detected in conversation messages.

use once_cell::sync::Lazy;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// Global syntax set for highlighting.
static SYNTAX_SET: Lazy<SyntaxSet> = Lazy::new(SyntaxSet::load_defaults_newlines);

/// Global theme set for highlighting.
static THEME_SET: Lazy<ThemeSet> = Lazy::new(ThemeSet::load_defaults);

/// Syntax highlighter for code blocks.
#[derive(Debug)]
pub struct SyntaxHighlighter {
    /// Theme name to use.
    theme_name: String,
}

impl Default for SyntaxHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl SyntaxHighlighter {
    /// Create a new syntax highlighter with default theme.
    #[must_use]
    pub fn new() -> Self {
        Self {
            theme_name: "base16-ocean.dark".to_string(),
        }
    }

    /// Highlight a code block and return styled lines.
    #[must_use]
    pub fn highlight_code(&self, code: &str, language: Option<&str>) -> Vec<Line<'static>> {
        let syntax = language
            .and_then(|lang| SYNTAX_SET.find_syntax_by_token(lang))
            .or_else(|| SYNTAX_SET.find_syntax_by_extension("rs"))
            .unwrap_or_else(|| SYNTAX_SET.find_syntax_plain_text());

        let theme = THEME_SET
            .themes
            .get(&self.theme_name)
            .or_else(|| THEME_SET.themes.values().next())
            .expect("syntect default theme set is never empty");

        let mut highlighter = HighlightLines::new(syntax, theme);
        let mut lines = Vec::new();

        for line in LinesWithEndings::from(code) {
            let Ok(ranges) = highlighter.highlight_line(line, &SYNTAX_SET) else {
                lines.push(Line::from(line.to_string()));
                continue;
            };

            let spans: Vec<Span<'static>> = ranges
                .into_iter()
                .map(|(style, text)| {
                    let fg = Color::Rgb(
                        style.foreground.r,
                        style.foreground.g,
                        style.foreground.b,
                    );

                    let mut ratatui_style = Style::default().fg(fg);

                    if style.font_style.contains(FontStyle::BOLD) {
                        ratatui_style = ratatui_style.add_modifier(Modifier::BOLD);
                    }
                    if style.font_style.contains(FontStyle::ITALIC) {
                        ratatui_style = ratatui_style.add_modifier(Modifier::ITALIC);
                    }
                    if style.font_style.contains(FontStyle::UNDERLINE) {
                        ratatui_style = ratatui_style.add_modifier(Modifier::UNDERLINED);
                    }

                    Span::styled(text.to_string(), ratatui_style)
                })
                .collect();

            lines.push(Line::from(spans));
        }

        lines
    }

    /// Process text that may contain markdown code blocks.
    /// Returns styled lines with syntax highlighting applied to code blocks.
    #[must_use]
    pub fn highlight_markdown_text(&self, text: &str) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let mut in_code_block = false;
        let mut code_block_lang: Option<String> = None;
        let mut code_buffer = String::new();

        for line in text.lines() {
            if line.starts_with("```") {
                if in_code_block {
                    // End of code block - highlight accumulated code
                    let highlighted = self.highlight_code(&code_buffer, code_block_lang.as_deref());
                    // Add code block border
                    lines.push(Line::from(Span::styled(
                        "┌─ Code ────────────────────────────────",
                        Style::default().fg(Color::DarkGray),
                    )));
                    for code_line in highlighted {
                        let mut bordered_spans = vec![Span::styled(
                            "│ ",
                            Style::default().fg(Color::DarkGray),
                        )];
                        bordered_spans.extend(code_line.spans);
                        lines.push(Line::from(bordered_spans));
                    }
                    lines.push(Line::from(Span::styled(
                        "└───────────────────────────────────────",
                        Style::default().fg(Color::DarkGray),
                    )));

                    // Reset state
                    in_code_block = false;
                    code_block_lang = None;
                    code_buffer.clear();
                } else {
                    // Start of code block
                    in_code_block = true;
                    // Extract language from ``` marker
                    let lang = line.trim_start_matches('`').trim();
                    code_block_lang = if lang.is_empty() {
                        None
                    } else {
                        Some(lang.to_string())
                    };
                }
            } else if in_code_block {
                // Accumulate code
                if !code_buffer.is_empty() {
                    code_buffer.push('\n');
                }
                code_buffer.push_str(line);
            } else {
                // Regular text - apply inline code highlighting
                lines.push(Self::highlight_inline_code(line));
            }
        }

        // Handle unclosed code block
        if in_code_block && !code_buffer.is_empty() {
            let highlighted = self.highlight_code(&code_buffer, code_block_lang.as_deref());
            lines.push(Line::from(Span::styled(
                "┌─ Code ────────────────────────────────",
                Style::default().fg(Color::DarkGray),
            )));
            for code_line in highlighted {
                let mut bordered_spans = vec![Span::styled(
                    "│ ",
                    Style::default().fg(Color::DarkGray),
                )];
                bordered_spans.extend(code_line.spans);
                lines.push(Line::from(bordered_spans));
            }
            lines.push(Line::from(Span::styled(
                "└───────────────────────────────────────",
                Style::default().fg(Color::DarkGray),
            )));
        }

        lines
    }

    /// Highlight inline code (backtick-wrapped) in a single line.
    fn highlight_inline_code(line: &str) -> Line<'static> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut buffer = String::new();
        let mut in_inline_code = false;

        for c in line.chars() {
            if c == '`' && !in_inline_code {
                // Start of inline code
                if !buffer.is_empty() {
                    spans.push(Span::raw(buffer.clone()));
                    buffer.clear();
                }
                in_inline_code = true;
            } else if c == '`' && in_inline_code {
                // End of inline code
                if !buffer.is_empty() {
                    spans.push(Span::styled(
                        buffer.clone(),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ));
                    buffer.clear();
                }
                in_inline_code = false;
            } else {
                buffer.push(c);
            }
        }

        // Flush remaining buffer
        if !buffer.is_empty() {
            if in_inline_code {
                // Unclosed inline code
                spans.push(Span::styled(
                    format!("`{buffer}"),
                    Style::default().fg(Color::Cyan),
                ));
            } else {
                spans.push(Span::raw(buffer));
            }
        }

        Line::from(spans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_highlight_inline_code() {
        // "This is " + "code" (styled) + " and more " + "stuff" (styled) = 4 spans
        let line = SyntaxHighlighter::highlight_inline_code("This is `code` and more `stuff`");
        assert_eq!(line.spans.len(), 4);
    }

    #[test]
    fn test_highlight_code_block() {
        let highlighter = SyntaxHighlighter::new();
        let code = "fn main() {\n    println!(\"Hello\");\n}";
        let lines = highlighter.highlight_code(code, Some("rust"));
        assert!(!lines.is_empty());
    }

    #[test]
    fn test_highlight_markdown() {
        let highlighter = SyntaxHighlighter::new();
        let text = "Here is code:\n```rust\nlet x = 1;\n```\nDone.";
        let lines = highlighter.highlight_markdown_text(text);
        // Should have: "Here is code:", code block header, code line, code block footer, "Done."
        assert!(lines.len() >= 5);
    }
}
