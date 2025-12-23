//! HTML export for conversations.
//!
//! Generates standalone HTML documents from Claude Code conversations,
//! suitable for viewing in any web browser.

use std::io::Write;

use chrono::{DateTime, Utc};

use crate::analytics::SessionAnalytics;
use crate::error::Result;
use crate::model::{
    content::{ThinkingBlock, ToolResult, ToolUse},
    AssistantMessage, ContentBlock, LogEntry, SystemMessage, UserMessage,
};
use crate::reconstruction::Conversation;

use super::{ExportOptions, Exporter};

/// HTML exporter for conversations.
#[derive(Debug, Clone)]
pub struct HtmlExporter {
    /// Document title.
    title: Option<String>,
    /// Include inline CSS styles.
    inline_styles: bool,
    /// Include session summary header.
    include_header: bool,
    /// Use dark theme.
    dark_theme: bool,
    /// Collapse thinking blocks.
    collapse_thinking: bool,
    /// Collapse tool outputs.
    collapse_tools: bool,
}

impl Default for HtmlExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl HtmlExporter {
    /// Create a new HTML exporter.
    #[must_use]
    pub fn new() -> Self {
        Self {
            title: None,
            inline_styles: true,
            include_header: true,
            dark_theme: false,
            collapse_thinking: true,
            collapse_tools: true,
        }
    }

    /// Set document title.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Enable/disable inline styles.
    #[must_use]
    pub fn inline_styles(mut self, enable: bool) -> Self {
        self.inline_styles = enable;
        self
    }

    /// Include session header.
    #[must_use]
    pub fn with_header(mut self, include: bool) -> Self {
        self.include_header = include;
        self
    }

    /// Use dark theme.
    #[must_use]
    pub fn dark_theme(mut self, dark: bool) -> Self {
        self.dark_theme = dark;
        self
    }

    /// Collapse thinking blocks.
    #[must_use]
    pub fn collapse_thinking(mut self, collapse: bool) -> Self {
        self.collapse_thinking = collapse;
        self
    }

    /// Collapse tool outputs.
    #[must_use]
    pub fn collapse_tools(mut self, collapse: bool) -> Self {
        self.collapse_tools = collapse;
        self
    }

    /// Write the HTML header.
    fn write_document_start<W: Write>(
        &self,
        writer: &mut W,
        title: &str,
    ) -> Result<()> {
        writeln!(writer, "<!DOCTYPE html>")?;
        writeln!(writer, "<html lang=\"en\">")?;
        writeln!(writer, "<head>")?;
        writeln!(writer, "  <meta charset=\"UTF-8\">")?;
        writeln!(writer, "  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">")?;
        writeln!(writer, "  <meta name=\"generator\" content=\"claude-snatch {}\"", crate::VERSION)?;
        writeln!(writer, "  <title>{}</title>", escape_html(title))?;

        if self.inline_styles {
            self.write_styles(writer)?;
        }

        writeln!(writer, "</head>")?;
        writeln!(writer, "<body class=\"{}\">", if self.dark_theme { "dark" } else { "light" })?;
        writeln!(writer, "<main class=\"conversation\">")?;

        Ok(())
    }

    /// Write inline CSS styles.
    fn write_styles<W: Write>(&self, writer: &mut W) -> Result<()> {
        writeln!(writer, "  <style>")?;
        writeln!(writer, r#"
    :root {{
      --bg-color: #ffffff;
      --text-color: #1a1a1a;
      --user-bg: #e8f4fd;
      --assistant-bg: #f5f5f5;
      --system-bg: #fff3cd;
      --tool-bg: #f8f9fa;
      --thinking-bg: #f0f0f0;
      --border-color: #dee2e6;
      --code-bg: #f4f4f4;
      --accent-color: #0066cc;
    }}

    .dark {{
      --bg-color: #1a1a1a;
      --text-color: #e0e0e0;
      --user-bg: #1e3a5f;
      --assistant-bg: #2d2d2d;
      --system-bg: #3d3520;
      --tool-bg: #252525;
      --thinking-bg: #2a2a2a;
      --border-color: #404040;
      --code-bg: #2d2d2d;
      --accent-color: #4da6ff;
    }}

    * {{
      box-sizing: border-box;
    }}

    body {{
      font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif;
      line-height: 1.6;
      margin: 0;
      padding: 20px;
      background-color: var(--bg-color);
      color: var(--text-color);
    }}

    .conversation {{
      max-width: 900px;
      margin: 0 auto;
    }}

    .session-header {{
      border-bottom: 2px solid var(--border-color);
      margin-bottom: 30px;
      padding-bottom: 20px;
    }}

    .session-header h1 {{
      margin: 0 0 15px 0;
      font-size: 1.8em;
    }}

    .session-stats {{
      display: grid;
      grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
      gap: 10px;
    }}

    .stat-item {{
      background: var(--tool-bg);
      padding: 10px 15px;
      border-radius: 6px;
    }}

    .stat-label {{
      font-size: 0.8em;
      color: var(--text-color);
      opacity: 0.7;
    }}

    .stat-value {{
      font-size: 1.2em;
      font-weight: 600;
    }}

    .message {{
      margin-bottom: 20px;
      padding: 16px 20px;
      border-radius: 8px;
      border: 1px solid var(--border-color);
    }}

    .message-user {{
      background-color: var(--user-bg);
    }}

    .message-assistant {{
      background-color: var(--assistant-bg);
    }}

    .message-system {{
      background-color: var(--system-bg);
    }}

    .message-header {{
      display: flex;
      justify-content: space-between;
      align-items: center;
      margin-bottom: 12px;
      padding-bottom: 8px;
      border-bottom: 1px solid var(--border-color);
    }}

    .message-role {{
      font-weight: 600;
      text-transform: uppercase;
      font-size: 0.85em;
      letter-spacing: 0.05em;
    }}

    .message-timestamp {{
      font-size: 0.8em;
      opacity: 0.7;
    }}

    .message-content {{
      white-space: pre-wrap;
      word-wrap: break-word;
    }}

    .message-content p {{
      margin: 0 0 1em 0;
    }}

    .message-content p:last-child {{
      margin-bottom: 0;
    }}

    .tool-use, .tool-result {{
      background-color: var(--tool-bg);
      border: 1px solid var(--border-color);
      border-radius: 6px;
      margin: 12px 0;
    }}

    .tool-header {{
      padding: 10px 15px;
      border-bottom: 1px solid var(--border-color);
      cursor: pointer;
      display: flex;
      justify-content: space-between;
      align-items: center;
    }}

    .tool-header:hover {{
      background-color: var(--border-color);
    }}

    .tool-name {{
      font-weight: 600;
      color: var(--accent-color);
    }}

    .tool-body {{
      padding: 15px;
    }}

    .tool-body.collapsed {{
      display: none;
    }}

    .thinking {{
      background-color: var(--thinking-bg);
      border: 1px dashed var(--border-color);
      border-radius: 6px;
      margin: 12px 0;
      font-style: italic;
    }}

    .thinking-header {{
      padding: 10px 15px;
      border-bottom: 1px dashed var(--border-color);
      cursor: pointer;
    }}

    .thinking-body {{
      padding: 15px;
    }}

    .thinking-body.collapsed {{
      display: none;
    }}

    pre, code {{
      font-family: 'SF Mono', Monaco, Consolas, 'Liberation Mono', monospace;
      font-size: 0.9em;
    }}

    pre {{
      background-color: var(--code-bg);
      padding: 15px;
      border-radius: 6px;
      overflow-x: auto;
      margin: 12px 0;
    }}

    code {{
      background-color: var(--code-bg);
      padding: 2px 6px;
      border-radius: 3px;
    }}

    pre code {{
      background: none;
      padding: 0;
    }}

    .toggle-icon {{
      font-size: 0.8em;
      transition: transform 0.2s;
    }}

    .toggle-icon.collapsed {{
      transform: rotate(-90deg);
    }}
  "#)?;
        writeln!(writer, "  </style>")?;
        Ok(())
    }

    /// Write the HTML footer.
    fn write_document_end<W: Write>(&self, writer: &mut W) -> Result<()> {
        writeln!(writer, "</main>")?;

        // Add toggle script
        writeln!(writer, r#"<script>
document.querySelectorAll('.tool-header, .thinking-header').forEach(header => {{
  header.addEventListener('click', () => {{
    const body = header.nextElementSibling;
    const icon = header.querySelector('.toggle-icon');
    body.classList.toggle('collapsed');
    if (icon) icon.classList.toggle('collapsed');
  }});
}});
</script>"#)?;

        writeln!(writer, "</body>")?;
        writeln!(writer, "</html>")?;
        Ok(())
    }

    /// Write session header with stats.
    fn write_session_header<W: Write>(
        &self,
        writer: &mut W,
        conversation: &Conversation,
    ) -> Result<()> {
        if !self.include_header {
            return Ok(());
        }

        let analytics = SessionAnalytics::from_conversation(conversation);
        let summary = analytics.summary_report();

        writeln!(writer, "<header class=\"session-header\">")?;
        writeln!(writer, "  <h1>Claude Code Conversation</h1>")?;
        writeln!(writer, "  <div class=\"session-stats\">")?;

        // Messages
        writeln!(writer, "    <div class=\"stat-item\">")?;
        writeln!(writer, "      <div class=\"stat-label\">Messages</div>")?;
        writeln!(writer, "      <div class=\"stat-value\">{}</div>", summary.total_messages)?;
        writeln!(writer, "    </div>")?;

        // Tokens
        writeln!(writer, "    <div class=\"stat-item\">")?;
        writeln!(writer, "      <div class=\"stat-label\">Tokens</div>")?;
        writeln!(writer, "      <div class=\"stat-value\">{}</div>", summary.total_tokens)?;
        writeln!(writer, "    </div>")?;

        // Tool invocations
        if summary.tool_invocations > 0 {
            writeln!(writer, "    <div class=\"stat-item\">")?;
            writeln!(writer, "      <div class=\"stat-label\">Tool Uses</div>")?;
            writeln!(writer, "      <div class=\"stat-value\">{}</div>", summary.tool_invocations)?;
            writeln!(writer, "    </div>")?;
        }

        // Duration
        if let Some(duration) = summary.duration {
            let secs = duration.num_seconds();
            let duration_str = if secs < 60 {
                format!("{}s", secs)
            } else if secs < 3600 {
                format!("{}m {}s", secs / 60, secs % 60)
            } else {
                format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
            };
            writeln!(writer, "    <div class=\"stat-item\">")?;
            writeln!(writer, "      <div class=\"stat-label\">Duration</div>")?;
            writeln!(writer, "      <div class=\"stat-value\">{}</div>", duration_str)?;
            writeln!(writer, "    </div>")?;
        }

        // Cost
        if let Some(cost) = summary.estimated_cost {
            writeln!(writer, "    <div class=\"stat-item\">")?;
            writeln!(writer, "      <div class=\"stat-label\">Est. Cost</div>")?;
            writeln!(writer, "      <div class=\"stat-value\">${:.4}</div>", cost)?;
            writeln!(writer, "    </div>")?;
        }

        writeln!(writer, "  </div>")?;
        writeln!(writer, "</header>")?;

        Ok(())
    }

    /// Write a user message.
    fn write_user_message<W: Write>(
        &self,
        writer: &mut W,
        user: &UserMessage,
        options: &ExportOptions,
    ) -> Result<()> {
        writeln!(writer, "<article class=\"message message-user\">")?;
        writeln!(writer, "  <div class=\"message-header\">")?;
        writeln!(writer, "    <span class=\"message-role\">User</span>")?;
        if options.include_timestamps {
            writeln!(writer, "    <span class=\"message-timestamp\">{}</span>",
                format_timestamp(&user.timestamp))?;
        }
        writeln!(writer, "  </div>")?;

        writeln!(writer, "  <div class=\"message-content\">")?;

        // Write text content
        if let Some(text) = user.message.as_text() {
            writeln!(writer, "    <p>{}</p>", escape_html(text))?;
        }

        // Write tool results if included
        if options.include_tool_results {
            for result in user.message.tool_results() {
                self.write_tool_result(writer, result)?;
            }
        }

        writeln!(writer, "  </div>")?;
        writeln!(writer, "</article>")?;

        Ok(())
    }

    /// Write an assistant message.
    fn write_assistant_message<W: Write>(
        &self,
        writer: &mut W,
        assistant: &AssistantMessage,
        options: &ExportOptions,
    ) -> Result<()> {
        writeln!(writer, "<article class=\"message message-assistant\">")?;
        writeln!(writer, "  <div class=\"message-header\">")?;
        writeln!(writer, "    <span class=\"message-role\">Assistant</span>")?;
        if options.include_timestamps {
            writeln!(writer, "    <span class=\"message-timestamp\">{}</span>",
                format_timestamp(&assistant.timestamp))?;
        }
        writeln!(writer, "  </div>")?;

        writeln!(writer, "  <div class=\"message-content\">")?;

        for content in &assistant.message.content {
            match content {
                ContentBlock::Text(text) => {
                    writeln!(writer, "    <p>{}</p>", escape_html(&text.text))?;
                }
                ContentBlock::Thinking(thinking) if options.include_thinking => {
                    self.write_thinking(writer, thinking)?;
                }
                ContentBlock::ToolUse(tool_use) if options.include_tool_use => {
                    self.write_tool_use(writer, tool_use)?;
                }
                _ => {}
            }
        }

        writeln!(writer, "  </div>")?;
        writeln!(writer, "</article>")?;

        Ok(())
    }

    /// Write a system message.
    fn write_system_message<W: Write>(
        &self,
        writer: &mut W,
        system: &SystemMessage,
        options: &ExportOptions,
    ) -> Result<()> {
        writeln!(writer, "<article class=\"message message-system\">")?;
        writeln!(writer, "  <div class=\"message-header\">")?;
        writeln!(writer, "    <span class=\"message-role\">System</span>")?;
        if options.include_timestamps {
            writeln!(writer, "    <span class=\"message-timestamp\">{}</span>",
                format_timestamp(&system.timestamp))?;
        }
        writeln!(writer, "  </div>")?;

        if let Some(content) = &system.content {
            writeln!(writer, "  <div class=\"message-content\">")?;
            writeln!(writer, "    <p>{}</p>", escape_html(content))?;
            writeln!(writer, "  </div>")?;
        }

        writeln!(writer, "</article>")?;

        Ok(())
    }

    /// Write a thinking block.
    fn write_thinking<W: Write>(&self, writer: &mut W, thinking: &ThinkingBlock) -> Result<()> {
        let collapsed_class = if self.collapse_thinking { " collapsed" } else { "" };
        let icon_class = if self.collapse_thinking { " collapsed" } else { "" };

        writeln!(writer, "    <div class=\"thinking\">")?;
        writeln!(writer, "      <div class=\"thinking-header\">")?;
        writeln!(writer, "        <span>Thinking</span>")?;
        writeln!(writer, "        <span class=\"toggle-icon{}\">▼</span>", icon_class)?;
        writeln!(writer, "      </div>")?;
        writeln!(writer, "      <div class=\"thinking-body{}\">", collapsed_class)?;
        writeln!(writer, "        <p>{}</p>", escape_html(&thinking.thinking))?;
        writeln!(writer, "      </div>")?;
        writeln!(writer, "    </div>")?;

        Ok(())
    }

    /// Write a tool use block.
    fn write_tool_use<W: Write>(&self, writer: &mut W, tool_use: &ToolUse) -> Result<()> {
        let collapsed_class = if self.collapse_tools { " collapsed" } else { "" };
        let icon_class = if self.collapse_tools { " collapsed" } else { "" };

        writeln!(writer, "    <div class=\"tool-use\">")?;
        writeln!(writer, "      <div class=\"tool-header\">")?;
        writeln!(writer, "        <span class=\"tool-name\">Tool: {}</span>", escape_html(&tool_use.name))?;
        writeln!(writer, "        <span class=\"toggle-icon{}\">▼</span>", icon_class)?;
        writeln!(writer, "      </div>")?;
        writeln!(writer, "      <div class=\"tool-body{}\">", collapsed_class)?;
        writeln!(writer, "        <pre><code>{}</code></pre>",
            escape_html(&serde_json::to_string_pretty(&tool_use.input).unwrap_or_default()))?;
        writeln!(writer, "      </div>")?;
        writeln!(writer, "    </div>")?;

        Ok(())
    }

    /// Write a tool result block.
    fn write_tool_result<W: Write>(&self, writer: &mut W, result: &ToolResult) -> Result<()> {
        let collapsed_class = if self.collapse_tools { " collapsed" } else { "" };
        let icon_class = if self.collapse_tools { " collapsed" } else { "" };

        let status = if result.is_explicit_error() { "Error" } else { "Result" };

        writeln!(writer, "    <div class=\"tool-result\">")?;
        writeln!(writer, "      <div class=\"tool-header\">")?;
        writeln!(writer, "        <span class=\"tool-name\">Tool {}</span>", status)?;
        writeln!(writer, "        <span class=\"toggle-icon{}\">▼</span>", icon_class)?;
        writeln!(writer, "      </div>")?;
        writeln!(writer, "      <div class=\"tool-body{}\">", collapsed_class)?;
        let content = result.content_as_string().unwrap_or("[complex content]");
        writeln!(writer, "        <pre><code>{}</code></pre>",
            escape_html(content))?;
        writeln!(writer, "      </div>")?;
        writeln!(writer, "    </div>")?;

        Ok(())
    }
}

impl Exporter for HtmlExporter {
    fn export_conversation<W: Write>(
        &self,
        conversation: &Conversation,
        writer: &mut W,
        options: &ExportOptions,
    ) -> Result<()> {
        let title = self.title.clone().unwrap_or_else(|| "Claude Code Conversation".to_string());
        self.write_document_start(writer, &title)?;
        self.write_session_header(writer, conversation)?;

        // Get entries based on options
        let entries = if options.main_thread_only {
            conversation.main_thread_entries()
        } else {
            conversation.chronological_entries()
        };

        for entry in entries {
            match entry {
                LogEntry::User(user) => {
                    self.write_user_message(writer, user, options)?;
                }
                LogEntry::Assistant(assistant) => {
                    self.write_assistant_message(writer, assistant, options)?;
                }
                LogEntry::System(system) if options.include_system => {
                    self.write_system_message(writer, system, options)?;
                }
                _ => {}
            }
        }

        self.write_document_end(writer)?;
        Ok(())
    }

    fn export_entries<W: Write>(
        &self,
        entries: &[LogEntry],
        writer: &mut W,
        options: &ExportOptions,
    ) -> Result<()> {
        let title = self.title.clone().unwrap_or_else(|| "Claude Code Conversation".to_string());
        self.write_document_start(writer, &title)?;

        for entry in entries {
            match entry {
                LogEntry::User(user) => {
                    self.write_user_message(writer, user, options)?;
                }
                LogEntry::Assistant(assistant) => {
                    self.write_assistant_message(writer, assistant, options)?;
                }
                LogEntry::System(system) if options.include_system => {
                    self.write_system_message(writer, system, options)?;
                }
                _ => {}
            }
        }

        self.write_document_end(writer)?;
        Ok(())
    }
}

/// Escape HTML special characters.
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Format a timestamp for display.
fn format_timestamp(ts: &DateTime<Utc>) -> String {
    ts.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_html() {
        assert_eq!(escape_html("<script>"), "&lt;script&gt;");
        assert_eq!(escape_html("a & b"), "a &amp; b");
        assert_eq!(escape_html("\"quoted\""), "&quot;quoted&quot;");
    }

    #[test]
    fn test_html_exporter_builder() {
        let exporter = HtmlExporter::new()
            .with_title("My Conversation")
            .dark_theme(true)
            .collapse_thinking(false);

        assert_eq!(exporter.title, Some("My Conversation".to_string()));
        assert!(exporter.dark_theme);
        assert!(!exporter.collapse_thinking);
    }
}
