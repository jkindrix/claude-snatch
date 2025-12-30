//! SQLite export for conversation data.
//!
//! Exports conversations to a normalized SQLite database for
//! querying and analysis. Supports full-text search and
//! maintains referential integrity.

use std::io::Write;
use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};

use crate::analytics::SessionAnalytics;
use crate::error::{Result, SnatchError};
use crate::model::{content::ToolResultContent, ContentBlock, LogEntry};
use crate::reconstruction::Conversation;

use super::{ExportOptions, Exporter};

/// SQLite exporter for conversation data.
#[derive(Debug)]
pub struct SqliteExporter {
    /// Enable full-text search indexes.
    enable_fts: bool,
    /// Create foreign key constraints.
    enable_foreign_keys: bool,
    /// Include usage statistics table.
    include_usage: bool,
}

impl Default for SqliteExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl SqliteExporter {
    /// Create a new SQLite exporter.
    #[must_use]
    pub fn new() -> Self {
        Self {
            enable_fts: true,
            enable_foreign_keys: true,
            include_usage: true,
        }
    }

    /// Enable or disable full-text search.
    #[must_use]
    pub fn with_fts(mut self, enable: bool) -> Self {
        self.enable_fts = enable;
        self
    }

    /// Enable or disable foreign key constraints.
    #[must_use]
    pub fn with_foreign_keys(mut self, enable: bool) -> Self {
        self.enable_foreign_keys = enable;
        self
    }

    /// Include usage statistics table.
    #[must_use]
    pub fn with_usage(mut self, include: bool) -> Self {
        self.include_usage = include;
        self
    }

    /// Export a conversation to a SQLite database file.
    pub fn export_to_file(
        &self,
        conversation: &Conversation,
        path: impl AsRef<Path>,
        options: &ExportOptions,
    ) -> Result<()> {
        let path = path.as_ref();

        // Remove existing file if present
        if path.exists() {
            std::fs::remove_file(path).map_err(|e| {
                SnatchError::io(
                    format!("Failed to remove existing database: {}", path.display()),
                    e,
                )
            })?;
        }

        let conn = Connection::open(path).map_err(|e| {
            SnatchError::export(format!("Failed to create SQLite database: {}", e))
        })?;

        self.export_to_connection(conversation, &conn, options)
    }

    /// Export a conversation to an existing SQLite connection.
    pub fn export_to_connection(
        &self,
        conversation: &Conversation,
        conn: &Connection,
        options: &ExportOptions,
    ) -> Result<()> {
        // Skip empty conversations (no exportable messages)
        if conversation.is_empty() {
            return Ok(());
        }

        // Enable foreign keys if requested
        if self.enable_foreign_keys {
            conn.execute_batch("PRAGMA foreign_keys = ON;")
                .map_err(|e| SnatchError::export(format!("Failed to enable foreign keys: {}", e)))?;
        }

        // Create schema
        self.create_schema(conn)?;

        // Start transaction for better performance
        conn.execute_batch("BEGIN TRANSACTION;")
            .map_err(|e| SnatchError::export(format!("Failed to begin transaction: {}", e)))?;

        // Insert session metadata
        let session_id = self.insert_session(conn, conversation)?;

        // Insert messages
        let entries = if options.main_thread_only {
            conversation.main_thread_entries()
        } else {
            conversation.chronological_entries()
        };

        for entry in entries {
            self.insert_entry(conn, session_id, entry, options)?;
        }

        // Insert usage statistics if enabled
        if self.include_usage && options.include_usage {
            self.insert_usage_stats(conn, session_id, conversation)?;
        }

        // Commit transaction
        conn.execute_batch("COMMIT;")
            .map_err(|e| SnatchError::export(format!("Failed to commit transaction: {}", e)))?;

        // Create FTS index if enabled
        if self.enable_fts {
            self.create_fts_index(conn)?;
        }

        // Optimize database
        conn.execute_batch("ANALYZE;")
            .map_err(|e| SnatchError::export(format!("Failed to analyze database: {}", e)))?;

        Ok(())
    }

    /// Create the database schema.
    fn create_schema(&self, conn: &Connection) -> Result<()> {
        let schema = r#"
            -- Sessions table
            CREATE TABLE IF NOT EXISTS sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT,
                version TEXT,
                start_time TEXT,
                end_time TEXT,
                duration_seconds REAL,
                created_at TEXT DEFAULT CURRENT_TIMESTAMP
            );

            -- Messages table
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_fk INTEGER NOT NULL,
                uuid TEXT,
                parent_uuid TEXT,
                message_type TEXT NOT NULL,
                role TEXT,
                model TEXT,
                timestamp TEXT,
                content TEXT,
                FOREIGN KEY (session_fk) REFERENCES sessions(id) ON DELETE CASCADE
            );

            -- Content blocks table
            CREATE TABLE IF NOT EXISTS content_blocks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                message_fk INTEGER NOT NULL,
                block_type TEXT NOT NULL,
                content TEXT,
                block_order INTEGER,
                FOREIGN KEY (message_fk) REFERENCES messages(id) ON DELETE CASCADE
            );

            -- Thinking blocks table
            CREATE TABLE IF NOT EXISTS thinking_blocks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                message_fk INTEGER NOT NULL,
                signature TEXT,
                thinking TEXT,
                block_order INTEGER,
                FOREIGN KEY (message_fk) REFERENCES messages(id) ON DELETE CASCADE
            );

            -- Tool uses table
            CREATE TABLE IF NOT EXISTS tool_uses (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                message_fk INTEGER NOT NULL,
                tool_use_id TEXT,
                tool_name TEXT NOT NULL,
                input_json TEXT,
                block_order INTEGER,
                FOREIGN KEY (message_fk) REFERENCES messages(id) ON DELETE CASCADE
            );

            -- Tool results table
            CREATE TABLE IF NOT EXISTS tool_results (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                message_fk INTEGER NOT NULL,
                tool_use_id TEXT,
                is_error INTEGER DEFAULT 0,
                status TEXT,
                output TEXT,
                block_order INTEGER,
                FOREIGN KEY (message_fk) REFERENCES messages(id) ON DELETE CASCADE
            );

            -- Usage statistics table
            CREATE TABLE IF NOT EXISTS usage_stats (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_fk INTEGER NOT NULL,
                total_messages INTEGER,
                user_messages INTEGER,
                assistant_messages INTEGER,
                total_tokens INTEGER,
                input_tokens INTEGER,
                output_tokens INTEGER,
                cache_read_tokens INTEGER,
                cache_creation_tokens INTEGER,
                cache_hit_rate REAL,
                tool_invocations INTEGER,
                thinking_blocks INTEGER,
                primary_model TEXT,
                estimated_cost_usd REAL,
                FOREIGN KEY (session_fk) REFERENCES sessions(id) ON DELETE CASCADE
            );

            -- Tool usage breakdown table
            CREATE TABLE IF NOT EXISTS tool_usage (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_fk INTEGER NOT NULL,
                tool_name TEXT NOT NULL,
                invocation_count INTEGER,
                FOREIGN KEY (session_fk) REFERENCES sessions(id) ON DELETE CASCADE
            );

            -- Create indexes
            CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_fk);
            CREATE INDEX IF NOT EXISTS idx_messages_uuid ON messages(uuid);
            CREATE INDEX IF NOT EXISTS idx_messages_type ON messages(message_type);
            CREATE INDEX IF NOT EXISTS idx_content_blocks_message ON content_blocks(message_fk);
            CREATE INDEX IF NOT EXISTS idx_thinking_blocks_message ON thinking_blocks(message_fk);
            CREATE INDEX IF NOT EXISTS idx_tool_uses_message ON tool_uses(message_fk);
            CREATE INDEX IF NOT EXISTS idx_tool_uses_name ON tool_uses(tool_name);
            CREATE INDEX IF NOT EXISTS idx_tool_uses_id ON tool_uses(tool_use_id);
            CREATE INDEX IF NOT EXISTS idx_tool_results_message ON tool_results(message_fk);
            CREATE INDEX IF NOT EXISTS idx_tool_results_tool_use_id ON tool_results(tool_use_id);
            CREATE INDEX IF NOT EXISTS idx_tool_results_is_error ON tool_results(is_error);
        "#;

        conn.execute_batch(schema)
            .map_err(|e| SnatchError::export(format!("Failed to create schema: {}", e)))?;

        Ok(())
    }

    /// Create full-text search index.
    fn create_fts_index(&self, conn: &Connection) -> Result<()> {
        let fts_sql = r#"
            -- Create FTS virtual table for messages
            CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                content,
                content='messages',
                content_rowid='id'
            );

            -- Populate FTS index
            INSERT INTO messages_fts(rowid, content)
            SELECT id, content FROM messages WHERE content IS NOT NULL;

            -- Create FTS virtual table for thinking
            CREATE VIRTUAL TABLE IF NOT EXISTS thinking_fts USING fts5(
                thinking,
                content='thinking_blocks',
                content_rowid='id'
            );

            -- Populate thinking FTS index
            INSERT INTO thinking_fts(rowid, thinking)
            SELECT id, thinking FROM thinking_blocks WHERE thinking IS NOT NULL;
        "#;

        conn.execute_batch(fts_sql)
            .map_err(|e| SnatchError::export(format!("Failed to create FTS index: {}", e)))?;

        Ok(())
    }

    /// Insert session metadata.
    fn insert_session(&self, conn: &Connection, conversation: &Conversation) -> Result<i64> {
        let analytics = SessionAnalytics::from_conversation(conversation);

        // Try main thread first, then fall back to chronological entries
        let session_id = conversation
            .main_thread_entries()
            .first()
            .and_then(|e| e.session_id())
            .or_else(|| {
                conversation
                    .chronological_entries()
                    .iter()
                    .find_map(|e| e.session_id())
            })
            .map(String::from);

        let version = conversation
            .main_thread_entries()
            .first()
            .and_then(|e| e.version())
            .or_else(|| {
                conversation
                    .chronological_entries()
                    .iter()
                    .find_map(|e| e.version())
            })
            .map(String::from);

        let start_time = analytics.start_time.map(|t| format_timestamp(&t));
        let end_time = analytics.end_time.map(|t| format_timestamp(&t));
        let duration = analytics.duration().map(|d| d.num_seconds() as f64);

        conn.execute(
            "INSERT INTO sessions (session_id, version, start_time, end_time, duration_seconds)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session_id, version, start_time, end_time, duration],
        )
        .map_err(|e| SnatchError::export(format!("Failed to insert session: {}", e)))?;

        Ok(conn.last_insert_rowid())
    }

    /// Insert a log entry.
    fn insert_entry(
        &self,
        conn: &Connection,
        session_fk: i64,
        entry: &LogEntry,
        options: &ExportOptions,
    ) -> Result<()> {
        match entry {
            LogEntry::User(user) if options.should_include_user() => {
                let uuid = entry.uuid();
                let parent_uuid = entry.parent_uuid();
                let timestamp = format_timestamp(&user.timestamp);
                let content = user.message.as_text().map(String::from);

                conn.execute(
                    "INSERT INTO messages (session_fk, uuid, parent_uuid, message_type, role, timestamp, content)
                     VALUES (?1, ?2, ?3, 'user', 'user', ?4, ?5)",
                    params![session_fk, uuid, parent_uuid, timestamp, content],
                )
                .map_err(|e| SnatchError::export(format!("Failed to insert user message: {}", e)))?;

                let message_id = conn.last_insert_rowid();

                // Insert content blocks for user messages
                match &user.message {
                    crate::model::message::UserContent::Simple(simple) => {
                        // Add text content to content_blocks for schema consistency
                        if !simple.content.is_empty() {
                            conn.execute(
                                "INSERT INTO content_blocks (message_fk, block_type, content, block_order)
                                 VALUES (?1, 'text', ?2, 0)",
                                params![message_id, simple.content],
                            )
                            .map_err(|e| {
                                SnatchError::export(format!("Failed to insert user text block: {}", e))
                            })?;
                        }
                    }
                    crate::model::message::UserContent::Blocks(blocks) => {
                        // Extract content blocks from user messages (tool results, images, text)
                        for (order, block) in blocks.content.iter().enumerate() {
                            self.insert_content_block(conn, message_id, block, order as i32, options)?;
                        }
                    }
                }
            }
            LogEntry::Assistant(assistant) if options.should_include_assistant() => {
                let uuid = entry.uuid();
                let parent_uuid = entry.parent_uuid();
                let timestamp = format_timestamp(&assistant.timestamp);
                let model = &assistant.message.model;

                // Collect text content
                let text_content: String = assistant
                    .message
                    .content
                    .iter()
                    .filter_map(|block| {
                        if let ContentBlock::Text(t) = block {
                            Some(t.text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                conn.execute(
                    "INSERT INTO messages (session_fk, uuid, parent_uuid, message_type, role, model, timestamp, content)
                     VALUES (?1, ?2, ?3, 'assistant', 'assistant', ?4, ?5, ?6)",
                    params![session_fk, uuid, parent_uuid, model, timestamp, text_content],
                )
                .map_err(|e| {
                    SnatchError::export(format!("Failed to insert assistant message: {}", e))
                })?;

                let message_id = conn.last_insert_rowid();

                // Insert content blocks
                for (order, block) in assistant.message.content.iter().enumerate() {
                    self.insert_content_block(conn, message_id, block, order as i32, options)?;
                }
            }
            LogEntry::System(system) if options.should_include_system() => {
                let uuid = entry.uuid();
                let timestamp = format_timestamp(&system.timestamp);
                let content = system.content.as_deref();

                conn.execute(
                    "INSERT INTO messages (session_fk, uuid, message_type, role, timestamp, content)
                     VALUES (?1, ?2, 'system', 'system', ?3, ?4)",
                    params![session_fk, uuid, timestamp, content],
                )
                .map_err(|e| {
                    SnatchError::export(format!("Failed to insert system message: {}", e))
                })?;
            }
            LogEntry::Summary(summary) if options.should_include_summary() => {
                let uuid = entry.uuid();

                conn.execute(
                    "INSERT INTO messages (session_fk, uuid, message_type, role, content)
                     VALUES (?1, ?2, 'summary', 'system', ?3)",
                    params![session_fk, uuid, summary.summary],
                )
                .map_err(|e| SnatchError::export(format!("Failed to insert summary: {}", e)))?;
            }
            _ => {
                // Skip other entry types or filtered entries
            }
        }

        Ok(())
    }

    /// Insert a content block.
    fn insert_content_block(
        &self,
        conn: &Connection,
        message_fk: i64,
        block: &ContentBlock,
        order: i32,
        options: &ExportOptions,
    ) -> Result<()> {
        match block {
            ContentBlock::Text(text) => {
                conn.execute(
                    "INSERT INTO content_blocks (message_fk, block_type, content, block_order)
                     VALUES (?1, 'text', ?2, ?3)",
                    params![message_fk, text.text, order],
                )
                .map_err(|e| SnatchError::export(format!("Failed to insert text block: {}", e)))?;
            }
            ContentBlock::Thinking(thinking) => {
                if options.should_include_thinking() {
                    conn.execute(
                        "INSERT INTO thinking_blocks (message_fk, signature, thinking, block_order)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![message_fk, thinking.signature, thinking.thinking, order],
                    )
                    .map_err(|e| {
                        SnatchError::export(format!("Failed to insert thinking block: {}", e))
                    })?;
                }
            }
            ContentBlock::ToolUse(tool_use) => {
                if options.should_include_tool_use() {
                    let input_json = serde_json::to_string(&tool_use.input).unwrap_or_default();

                    conn.execute(
                        "INSERT INTO tool_uses (message_fk, tool_use_id, tool_name, input_json, block_order)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![message_fk, tool_use.id, tool_use.name, input_json, order],
                    )
                    .map_err(|e| {
                        SnatchError::export(format!("Failed to insert tool use: {}", e))
                    })?;
                }
            }
            ContentBlock::ToolResult(result) => {
                if options.should_include_tool_results() {
                    let is_error = result.is_explicit_error();
                    let status = if is_error {
                        "error"
                    } else if result.is_implicit_success() {
                        "success_implicit"
                    } else {
                        "success"
                    };

                    let output = result.content.as_ref().map(|c| match c {
                        ToolResultContent::String(s) => s.clone(),
                        ToolResultContent::Array(arr) => {
                            serde_json::to_string(arr).unwrap_or_default()
                        }
                    });

                    conn.execute(
                        "INSERT INTO tool_results (message_fk, tool_use_id, is_error, status, output, block_order)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![message_fk, result.tool_use_id, is_error as i32, status, output, order],
                    )
                    .map_err(|e| {
                        SnatchError::export(format!("Failed to insert tool result: {}", e))
                    })?;
                }
            }
            ContentBlock::Image(_) => {
                if options.include_images {
                    conn.execute(
                        "INSERT INTO content_blocks (message_fk, block_type, content, block_order)
                         VALUES (?1, 'image', '[image data]', ?2)",
                        params![message_fk, order],
                    )
                    .map_err(|e| {
                        SnatchError::export(format!("Failed to insert image block: {}", e))
                    })?;
                }
            }
        }

        Ok(())
    }

    /// Insert usage statistics.
    fn insert_usage_stats(
        &self,
        conn: &Connection,
        session_fk: i64,
        conversation: &Conversation,
    ) -> Result<()> {
        let analytics = SessionAnalytics::from_conversation(conversation);
        let summary = analytics.summary_report();

        // Get cache token info from aggregated usage
        let cache_read = analytics.usage.usage.cache_read_input_tokens;
        let cache_creation = analytics.usage.usage.cache_creation_input_tokens;

        conn.execute(
            "INSERT INTO usage_stats (
                session_fk, total_messages, user_messages, assistant_messages,
                total_tokens, input_tokens, output_tokens, cache_read_tokens,
                cache_creation_tokens, cache_hit_rate, tool_invocations,
                thinking_blocks, primary_model, estimated_cost_usd
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                session_fk,
                summary.total_messages,
                summary.user_messages,
                summary.assistant_messages,
                summary.total_tokens,
                summary.input_tokens,
                summary.output_tokens,
                cache_read,
                cache_creation,
                summary.cache_hit_rate,
                summary.tool_invocations,
                summary.thinking_blocks,
                summary.primary_model,
                summary.estimated_cost,
            ],
        )
        .map_err(|e| SnatchError::export(format!("Failed to insert usage stats: {}", e)))?;

        // Insert tool usage breakdown
        for (tool_name, count) in &analytics.tool_counts {
            conn.execute(
                "INSERT INTO tool_usage (session_fk, tool_name, invocation_count)
                 VALUES (?1, ?2, ?3)",
                params![session_fk, tool_name, count],
            )
            .map_err(|e| SnatchError::export(format!("Failed to insert tool usage: {}", e)))?;
        }

        Ok(())
    }
}

/// The Exporter trait implementation writes to a writer but SQLite needs a file.
/// This implementation writes an error message explaining this.
impl Exporter for SqliteExporter {
    fn export_conversation<W: Write>(
        &self,
        _conversation: &Conversation,
        writer: &mut W,
        _options: &ExportOptions,
    ) -> Result<()> {
        // SQLite export requires a file path, not a writer
        writeln!(
            writer,
            "SQLite export requires a file path. Use --output <path.db> to specify the database file."
        )?;
        Ok(())
    }

    fn export_entries<W: Write>(
        &self,
        _entries: &[LogEntry],
        writer: &mut W,
        _options: &ExportOptions,
    ) -> Result<()> {
        writeln!(
            writer,
            "SQLite export requires a file path. Use --output <path.db> to specify the database file."
        )?;
        Ok(())
    }
}

/// Format a timestamp for SQLite.
fn format_timestamp(ts: &DateTime<Utc>) -> String {
    ts.format("%Y-%m-%d %H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sqlite_exporter_builder() {
        let exporter = SqliteExporter::new()
            .with_fts(false)
            .with_foreign_keys(false);

        assert!(!exporter.enable_fts);
        assert!(!exporter.enable_foreign_keys);
    }

    #[test]
    fn test_sqlite_schema_has_tool_results_table() {
        let conn = Connection::open_in_memory().unwrap();
        let exporter = SqliteExporter::new().with_fts(false);
        exporter.create_schema(&conn).unwrap();

        // Verify tool_results table exists with correct columns
        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(tool_results)")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert!(columns.contains(&"tool_use_id".to_string()));
        assert!(columns.contains(&"is_error".to_string()));
        assert!(columns.contains(&"status".to_string()));
        assert!(columns.contains(&"output".to_string()));
    }

    #[test]
    fn test_sqlite_schema_has_tool_results_indexes() {
        let conn = Connection::open_in_memory().unwrap();
        let exporter = SqliteExporter::new().with_fts(false);
        exporter.create_schema(&conn).unwrap();

        // Verify indexes exist for efficient joins
        let indexes: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='index' AND name LIKE 'idx_tool_%'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert!(indexes.contains(&"idx_tool_uses_id".to_string()));
        assert!(indexes.contains(&"idx_tool_results_tool_use_id".to_string()));
        assert!(indexes.contains(&"idx_tool_results_is_error".to_string()));
    }
}
