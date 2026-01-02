//! HTML export for conversations.
//!
//! Generates standalone HTML documents from Claude Code conversations,
//! suitable for viewing in any web browser.

use std::io::Write;

use chrono::{DateTime, Utc};

use crate::analytics::SessionAnalytics;
use crate::error::Result;
use crate::model::{
    content::{ImageBlock, ImageSource, ThinkingBlock, ToolResult, ToolUse},
    AssistantMessage, ContentBlock, LogEntry, SystemMessage, UserMessage,
};
use crate::reconstruction::Conversation;

use super::{ContentType, ExportOptions, Exporter};

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
    /// Include table of contents / navigation sidebar.
    include_toc: bool,
    /// Inline images as base64 data URLs.
    inline_images: bool,
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
            include_toc: false,
            inline_images: true,
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

    /// Include table of contents / navigation sidebar.
    #[must_use]
    pub fn with_toc(mut self, include: bool) -> Self {
        self.include_toc = include;
        self
    }

    /// Inline images as base64 data URLs.
    #[must_use]
    pub fn inline_images(mut self, inline: bool) -> Self {
        self.inline_images = inline;
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
        writeln!(writer, "  <meta name=\"generator\" content=\"claude-snatch {}\">", crate::VERSION)?;
        writeln!(writer, "  <title>{}</title>", escape_html(title))?;

        if self.inline_styles {
            self.write_styles(writer)?;
        }

        writeln!(writer, "</head>")?;
        let body_class = if self.dark_theme { "dark" } else { "light" };
        let has_toc = if self.include_toc { " has-toc" } else { "" };
        writeln!(writer, "<body class=\"{}{}\">", body_class, has_toc)?;

        if self.include_toc {
            writeln!(writer, "<div class=\"layout-wrapper\">")?;
            // TOC will be inserted here after we know all entries
            writeln!(writer, "<nav class=\"toc\" id=\"toc\">")?;
            writeln!(writer, "  <div class=\"toc-header\">Contents</div>")?;
            writeln!(writer, "  <ul class=\"toc-list\" id=\"toc-list\">")?;
            writeln!(writer, "  </ul>")?;
            writeln!(writer, "</nav>")?;
        }

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

    /* Table of Contents styles */
    .has-toc {{
      padding: 0;
    }}

    .layout-wrapper {{
      display: flex;
      min-height: 100vh;
    }}

    .has-toc .conversation {{
      flex: 1;
      max-width: 900px;
      margin: 0;
      padding: 20px 40px;
    }}

    .toc {{
      position: sticky;
      top: 0;
      width: 280px;
      height: 100vh;
      overflow-y: auto;
      background: var(--tool-bg);
      border-right: 1px solid var(--border-color);
      padding: 0;
      flex-shrink: 0;
    }}

    .toc-header {{
      font-weight: 600;
      font-size: 1.1em;
      padding: 20px 16px 12px;
      border-bottom: 1px solid var(--border-color);
      position: sticky;
      top: 0;
      background: var(--tool-bg);
      z-index: 1;
    }}

    .toc-list {{
      list-style: none;
      margin: 0;
      padding: 8px 0;
    }}

    .toc-item {{
      margin: 0;
    }}

    .toc-link {{
      display: block;
      padding: 8px 16px;
      color: var(--text-color);
      text-decoration: none;
      font-size: 0.85em;
      border-left: 3px solid transparent;
      transition: all 0.15s ease;
    }}

    .toc-link:hover {{
      background: var(--border-color);
      border-left-color: var(--accent-color);
    }}

    .toc-link.active {{
      background: var(--border-color);
      border-left-color: var(--accent-color);
      font-weight: 500;
    }}

    .toc-role {{
      font-weight: 600;
      text-transform: uppercase;
      font-size: 0.75em;
      letter-spacing: 0.05em;
      opacity: 0.7;
      display: block;
      margin-bottom: 2px;
    }}

    .toc-preview {{
      display: block;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
      opacity: 0.8;
    }}

    /* Image styles */
    .message-image {{
      max-width: 100%;
      margin: 12px 0;
      border-radius: 6px;
      border: 1px solid var(--border-color);
    }}

    .message-image img {{
      display: block;
      max-width: 100%;
      height: auto;
      border-radius: 5px;
    }}

    .image-caption {{
      font-size: 0.85em;
      opacity: 0.7;
      margin-top: 6px;
      text-align: center;
    }}

    .image-error {{
      background: var(--tool-bg);
      border: 1px dashed var(--border-color);
      border-radius: 6px;
      padding: 20px;
      text-align: center;
      color: var(--text-color);
      opacity: 0.7;
    }}

    @media (max-width: 1000px) {{
      .layout-wrapper {{
        flex-direction: column;
      }}

      .toc {{
        position: relative;
        width: 100%;
        height: auto;
        max-height: 200px;
        border-right: none;
        border-bottom: 1px solid var(--border-color);
      }}

      .has-toc .conversation {{
        padding: 20px;
      }}
    }}
  "#)?;
        writeln!(writer, "  </style>")?;
        Ok(())
    }

    /// Write the HTML footer.
    fn write_document_end<W: Write>(&self, writer: &mut W) -> Result<()> {
        writeln!(writer, "</main>")?;

        if self.include_toc {
            writeln!(writer, "</div>")?; // Close layout-wrapper
        }

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

        // Add TOC script if enabled
        if self.include_toc {
            writeln!(writer, r#"<script>
(function() {{
  // Populate TOC from messages
  const tocList = document.getElementById('toc-list');
  const messages = document.querySelectorAll('.message');

  messages.forEach((msg, idx) => {{
    // Add ID to message for linking
    const msgId = 'msg-' + idx;
    msg.id = msgId;

    // Get role and preview
    const roleEl = msg.querySelector('.message-role');
    const contentEl = msg.querySelector('.message-content p');
    const role = roleEl ? roleEl.textContent : 'Message';
    let preview = contentEl ? contentEl.textContent.trim() : '';
    if (preview.length > 50) preview = preview.substring(0, 50) + '...';

    // Create TOC item
    const li = document.createElement('li');
    li.className = 'toc-item';
    const a = document.createElement('a');
    a.className = 'toc-link';
    a.href = '#' + msgId;
    a.innerHTML = '<span class="toc-role">' + role + '</span>' +
                  '<span class="toc-preview">' + preview + '</span>';
    li.appendChild(a);
    tocList.appendChild(li);
  }});

  // Highlight active TOC item on scroll
  const tocLinks = document.querySelectorAll('.toc-link');
  let ticking = false;

  function updateActiveLink() {{
    const scrollPos = window.scrollY + 100;
    let activeIdx = 0;

    messages.forEach((msg, idx) => {{
      if (msg.offsetTop <= scrollPos) {{
        activeIdx = idx;
      }}
    }});

    tocLinks.forEach((link, idx) => {{
      link.classList.toggle('active', idx === activeIdx);
    }});

    ticking = false;
  }}

  window.addEventListener('scroll', () => {{
    if (!ticking) {{
      window.requestAnimationFrame(updateActiveLink);
      ticking = true;
    }}
  }});

  // Initial update
  updateActiveLink();

  // Smooth scroll for TOC links
  tocLinks.forEach(link => {{
    link.addEventListener('click', (e) => {{
      e.preventDefault();
      const target = document.querySelector(link.getAttribute('href'));
      if (target) {{
        target.scrollIntoView({{ behavior: 'smooth', block: 'start' }});
      }}
    }});
  }});
}})();
</script>"#)?;
        }

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

        // Write images
        for image in user.message.images() {
            self.write_image(writer, image)?;
        }

        // Write tool results if included
        if options.should_include_tool_results() {
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
                    // Use should_include() directly for text content to respect exclusive filter
                    if options.should_include(ContentType::Assistant) {
                        writeln!(writer, "    <p>{}</p>", escape_html(&text.text))?;
                    }
                }
                ContentBlock::Thinking(thinking) if options.should_include_thinking() => {
                    self.write_thinking(writer, thinking)?;
                }
                ContentBlock::ToolUse(tool_use) if options.should_include_tool_use() => {
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

    /// Write an image block.
    fn write_image<W: Write>(&self, writer: &mut W, image: &ImageBlock) -> Result<()> {
        writeln!(writer, "    <div class=\"message-image\">")?;

        match &image.source {
            ImageSource::Base64 { media_type, data } if self.inline_images => {
                // Inline as data URL
                writeln!(writer, "      <img src=\"data:{};base64,{}\" alt=\"User-provided image\" />",
                    escape_html(media_type), data)?;
                if let Some(size) = image.source.approximate_size() {
                    let size_str = format_bytes(size);
                    writeln!(writer, "      <div class=\"image-caption\">{} image ({})</div>",
                        escape_html(media_type), size_str)?;
                }
            }
            ImageSource::Base64 { media_type, data } => {
                // Show placeholder with size info
                if let Some(size) = image.source.approximate_size() {
                    let size_str = format_bytes(size);
                    writeln!(writer, "      <div class=\"image-error\">")?;
                    writeln!(writer, "        [Image: {} ({}) - base64 data available]",
                        escape_html(media_type), size_str)?;
                    writeln!(writer, "      </div>")?;
                } else {
                    writeln!(writer, "      <div class=\"image-error\">")?;
                    writeln!(writer, "        [Image: {} - base64 data {}]",
                        escape_html(media_type), if data.is_empty() { "empty" } else { "available" })?;
                    writeln!(writer, "      </div>")?;
                }
            }
            ImageSource::Url { url } => {
                // External URL - embed or link based on settings
                if self.inline_images {
                    writeln!(writer, "      <img src=\"{}\" alt=\"External image\" />", escape_html(url))?;
                    writeln!(writer, "      <div class=\"image-caption\">Source: {}</div>", escape_html(url))?;
                } else {
                    writeln!(writer, "      <div class=\"image-error\">")?;
                    writeln!(writer, "        <a href=\"{}\" target=\"_blank\">[External image: {}]</a>",
                        escape_html(url), escape_html(url))?;
                    writeln!(writer, "      </div>")?;
                }
            }
            ImageSource::File { file_id } => {
                // Files API reference (beta feature) - show file ID
                writeln!(writer, "      <div class=\"image-error\">")?;
                writeln!(writer, "        [Files API image: {}]", escape_html(file_id))?;
                writeln!(writer, "      </div>")?;
            }
        }

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
                LogEntry::User(user) if options.should_include_user() => {
                    self.write_user_message(writer, user, options)?;
                }
                LogEntry::Assistant(assistant) if options.should_include_assistant() => {
                    self.write_assistant_message(writer, assistant, options)?;
                }
                LogEntry::System(system) if options.should_include_system() => {
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
                LogEntry::User(user) if options.should_include_user() => {
                    self.write_user_message(writer, user, options)?;
                }
                LogEntry::Assistant(assistant) if options.should_include_assistant() => {
                    self.write_assistant_message(writer, assistant, options)?;
                }
                LogEntry::System(system) if options.should_include_system() => {
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

/// Format bytes in human-readable form.
fn format_bytes(bytes: usize) -> String {
    const KB: usize = 1024;
    const MB: usize = 1024 * KB;

    if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
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

    #[test]
    fn test_html_exporter_with_toc() {
        let exporter = HtmlExporter::new()
            .with_toc(true)
            .dark_theme(false);

        assert!(exporter.include_toc);
        assert!(!exporter.dark_theme);
    }

    #[test]
    fn test_html_document_with_toc() {
        let exporter = HtmlExporter::new().with_toc(true);
        let mut output = Vec::new();

        exporter.write_document_start(&mut output, "Test").unwrap();

        let html = String::from_utf8(output).unwrap();
        assert!(html.contains("has-toc"));
        assert!(html.contains("layout-wrapper"));
        assert!(html.contains("toc-list"));
    }

    #[test]
    fn test_html_toc_styles() {
        let exporter = HtmlExporter::new().with_toc(true);
        let mut output = Vec::new();

        exporter.write_styles(&mut output).unwrap();

        let css = String::from_utf8(output).unwrap();
        assert!(css.contains(".toc"));
        assert!(css.contains(".toc-link"));
        assert!(css.contains(".toc-preview"));
    }

    #[test]
    fn test_html_toc_script() {
        let exporter = HtmlExporter::new().with_toc(true);
        let mut output = Vec::new();

        exporter.write_document_end(&mut output).unwrap();

        let html = String::from_utf8(output).unwrap();
        assert!(html.contains("tocList"));
        assert!(html.contains("scrollIntoView"));
        assert!(html.contains("updateActiveLink"));
    }

    #[test]
    fn test_html_exporter_inline_images() {
        let exporter = HtmlExporter::new()
            .inline_images(true);

        assert!(exporter.inline_images);

        let disabled = HtmlExporter::new()
            .inline_images(false);

        assert!(!disabled.inline_images);
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(1048576), "1.0 MB");
        assert_eq!(format_bytes(1572864), "1.5 MB");
    }

    #[test]
    fn test_html_image_styles() {
        let exporter = HtmlExporter::new();
        let mut output = Vec::new();

        exporter.write_styles(&mut output).unwrap();

        let css = String::from_utf8(output).unwrap();
        assert!(css.contains(".message-image"));
        assert!(css.contains(".image-caption"));
        assert!(css.contains(".image-error"));
    }

    #[test]
    fn test_write_image_base64() {
        use crate::model::content::{ImageBlock, ImageSource};

        let exporter = HtmlExporter::new().inline_images(true);
        let image = ImageBlock {
            source: ImageSource::Base64 {
                media_type: "image/png".to_string(),
                data: "iVBORw0KGgo=".to_string(),
            },
            extra: indexmap::IndexMap::new(),
        };

        let mut output = Vec::new();
        exporter.write_image(&mut output, &image).unwrap();

        let html = String::from_utf8(output).unwrap();
        assert!(html.contains("data:image/png;base64,iVBORw0KGgo="));
        assert!(html.contains("message-image"));
    }

    #[test]
    fn test_write_image_url() {
        use crate::model::content::{ImageBlock, ImageSource};

        let exporter = HtmlExporter::new().inline_images(true);
        let image = ImageBlock {
            source: ImageSource::Url {
                url: "https://example.com/image.png".to_string(),
            },
            extra: indexmap::IndexMap::new(),
        };

        let mut output = Vec::new();
        exporter.write_image(&mut output, &image).unwrap();

        let html = String::from_utf8(output).unwrap();
        assert!(html.contains("https://example.com/image.png"));
        assert!(html.contains("<img src="));
    }
}
