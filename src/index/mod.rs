//! Full-text search index using tantivy.
//!
//! This module provides high-performance full-text search indexing for
//! Claude Code conversation logs using the tantivy search engine.
//!
//! # Features
//!
//! - Fast full-text search across all sessions
//! - Incremental index updates
//! - Field-specific search (message type, model, tool)
//! - Index persistence and management

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Schema, Value, FAST, STORED, STRING, TEXT};
use tantivy::{doc, Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument};

use crate::discovery::Session;
use crate::error::{Result, SnatchError};
use crate::model::{ContentBlock, LogEntry};

/// Default number of search results.
pub const DEFAULT_RESULT_LIMIT: usize = 100;

/// Schema field names.
mod fields {
    pub const SESSION_ID: &str = "session_id";
    pub const PROJECT: &str = "project";
    pub const UUID: &str = "uuid";
    pub const TIMESTAMP: &str = "timestamp";
    pub const MESSAGE_TYPE: &str = "message_type";
    pub const MODEL: &str = "model";
    pub const CONTENT: &str = "content";
    pub const THINKING: &str = "thinking";
    pub const TOOL_NAME: &str = "tool_name";
    pub const TOOL_INPUT: &str = "tool_input";
}

/// A search index for Claude Code conversation logs.
pub struct SearchIndex {
    index: Index,
    schema: Schema,
    reader: IndexReader,
    writer: Arc<RwLock<IndexWriter>>,
    index_path: PathBuf,
}

/// A search result from the index.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchHit {
    /// Session ID containing the match.
    pub session_id: String,
    /// Project path.
    pub project: String,
    /// Message UUID.
    pub uuid: String,
    /// Timestamp of the message.
    pub timestamp: String,
    /// Message type (user, assistant, system, etc.).
    pub message_type: String,
    /// Model used (for assistant messages).
    pub model: Option<String>,
    /// Matched content snippet.
    pub content_snippet: String,
    /// Relevance score.
    pub score: f32,
}

/// Index statistics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexStats {
    /// Total number of indexed documents.
    pub document_count: u64,
    /// Total number of indexed sessions.
    pub session_count: usize,
    /// Index size on disk in bytes.
    pub size_bytes: u64,
    /// Last update timestamp.
    pub last_updated: Option<String>,
}

impl SearchIndex {
    /// Create the tantivy schema for conversation logs.
    fn build_schema() -> Schema {
        let mut schema_builder = Schema::builder();

        // Stored fields for result display
        schema_builder.add_text_field(fields::SESSION_ID, STRING | STORED);
        schema_builder.add_text_field(fields::PROJECT, STRING | STORED);
        schema_builder.add_text_field(fields::UUID, STRING | STORED);
        schema_builder.add_text_field(fields::TIMESTAMP, STRING | STORED | FAST);
        schema_builder.add_text_field(fields::MESSAGE_TYPE, STRING | STORED);
        schema_builder.add_text_field(fields::MODEL, STRING | STORED);

        // Searchable text fields
        schema_builder.add_text_field(fields::CONTENT, TEXT | STORED);
        schema_builder.add_text_field(fields::THINKING, TEXT);
        schema_builder.add_text_field(fields::TOOL_NAME, STRING | STORED);
        schema_builder.add_text_field(fields::TOOL_INPUT, TEXT);

        schema_builder.build()
    }

    /// Open or create a search index at the specified path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();

        // Ensure directory exists
        if !path.exists() {
            std::fs::create_dir_all(path).map_err(|e| {
                SnatchError::io(format!("Failed to create index directory: {}", path.display()), e)
            })?;
        }

        let schema = Self::build_schema();

        // Try to open existing index, or create new one
        let index = if path.join("meta.json").exists() {
            Index::open_in_dir(path).map_err(|e| {
                SnatchError::IndexError(format!("Failed to open index: {}", e))
            })?
        } else {
            Index::create_in_dir(path, schema.clone()).map_err(|e| {
                SnatchError::IndexError(format!("Failed to create index: {}", e))
            })?
        };

        // Create reader with reload on commit
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .map_err(|e| SnatchError::IndexError(format!("Failed to create reader: {}", e)))?;

        // Create writer with 50MB buffer
        let writer = index
            .writer(50_000_000)
            .map_err(|e| SnatchError::IndexError(format!("Failed to create writer: {}", e)))?;

        Ok(Self {
            index,
            schema,
            reader,
            writer: Arc::new(RwLock::new(writer)),
            index_path: path.to_path_buf(),
        })
    }

    /// Open or create the default search index.
    pub fn open_default() -> Result<Self> {
        Self::open_with_config(None)
    }

    /// Open or create a search index with optional config.
    pub fn open_with_config(config: Option<&crate::config::Config>) -> Result<Self> {
        // Check for configured index directory
        let index_dir = config
            .and_then(|c| c.index.directory.clone())
            .unwrap_or_else(|| {
                directories::ProjectDirs::from("", "", "claude-snatch")
                    .map(|d| d.cache_dir().to_path_buf())
                    .unwrap_or_else(|| PathBuf::from(".claude-snatch-cache"))
                    .join("search-index")
            });

        Self::open(index_dir)
    }

    /// Get the default index directory path.
    pub fn default_index_dir() -> PathBuf {
        directories::ProjectDirs::from("", "", "claude-snatch")
            .map(|d| d.cache_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".claude-snatch-cache"))
            .join("search-index")
    }

    /// Index a single log entry.
    fn index_entry(
        &self,
        writer: &mut IndexWriter,
        session_id: &str,
        project: &str,
        entry: &LogEntry,
    ) -> Result<()> {
        // Field lookups - these fields are added by build_schema() and always exist
        let session_id_field = self.schema.get_field(fields::SESSION_ID).expect("schema field");
        let project_field = self.schema.get_field(fields::PROJECT).expect("schema field");
        let uuid_field = self.schema.get_field(fields::UUID).expect("schema field");
        let timestamp_field = self.schema.get_field(fields::TIMESTAMP).expect("schema field");
        let message_type_field = self.schema.get_field(fields::MESSAGE_TYPE).expect("schema field");
        let model_field = self.schema.get_field(fields::MODEL).expect("schema field");
        let content_field = self.schema.get_field(fields::CONTENT).expect("schema field");
        let thinking_field = self.schema.get_field(fields::THINKING).expect("schema field");
        let tool_name_field = self.schema.get_field(fields::TOOL_NAME).expect("schema field");
        let tool_input_field = self.schema.get_field(fields::TOOL_INPUT).expect("schema field");

        let uuid = entry.uuid().unwrap_or("").to_string();
        let timestamp = entry.timestamp().map_or_else(String::new, |t| t.to_rfc3339());
        let message_type = entry.message_type().to_string();

        match entry {
            LogEntry::User(user) => {
                let content = match &user.message {
                    crate::model::UserContent::Simple(s) => s.content.clone(),
                    crate::model::UserContent::Blocks(b) => b
                        .content
                        .iter()
                        .filter_map(|c| match c {
                            ContentBlock::Text(t) => Some(t.text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                };

                writer.add_document(doc!(
                    session_id_field => session_id,
                    project_field => project,
                    uuid_field => uuid,
                    timestamp_field => timestamp,
                    message_type_field => message_type,
                    content_field => content
                )).map_err(|e| SnatchError::IndexError(format!("Failed to add document: {}", e)))?;
            }
            LogEntry::Assistant(assistant) => {
                let model = assistant.message.model.clone();

                let mut content_parts = Vec::new();
                let mut thinking_parts = Vec::new();
                let mut tool_names = Vec::new();
                let mut tool_inputs = Vec::new();

                for block in &assistant.message.content {
                    match block {
                        ContentBlock::Text(t) => content_parts.push(t.text.clone()),
                        ContentBlock::Thinking(t) => thinking_parts.push(t.thinking.clone()),
                        ContentBlock::ToolUse(t) => {
                            tool_names.push(t.name.clone());
                            if let Ok(input_str) = serde_json::to_string(&t.input) {
                                tool_inputs.push(input_str);
                            }
                        }
                        ContentBlock::ToolResult(r) => {
                            if let Some(crate::model::content::ToolResultContent::String(s)) =
                                &r.content
                            {
                                content_parts.push(s.clone());
                            }
                        }
                        _ => {}
                    }
                }

                let content = content_parts.join("\n");
                let thinking = thinking_parts.join("\n");

                let mut doc = doc!(
                    session_id_field => session_id,
                    project_field => project,
                    uuid_field => uuid,
                    timestamp_field => timestamp,
                    message_type_field => message_type,
                    model_field => model,
                    content_field => content,
                    thinking_field => thinking
                );

                for name in &tool_names {
                    doc.add_text(tool_name_field, name);
                }
                for input in &tool_inputs {
                    doc.add_text(tool_input_field, input);
                }

                writer.add_document(doc)
                    .map_err(|e| SnatchError::IndexError(format!("Failed to add document: {}", e)))?;
            }
            LogEntry::System(system) => {
                if let Some(content) = &system.content {
                    writer.add_document(doc!(
                        session_id_field => session_id,
                        project_field => project,
                        uuid_field => uuid,
                        timestamp_field => timestamp,
                        message_type_field => message_type,
                        content_field => content.clone()
                    )).map_err(|e| SnatchError::IndexError(format!("Failed to add document: {}", e)))?;
                }
            }
            LogEntry::Summary(summary) => {
                writer.add_document(doc!(
                    session_id_field => session_id,
                    project_field => project,
                    uuid_field => uuid,
                    timestamp_field => timestamp,
                    message_type_field => message_type,
                    content_field => summary.summary.clone()
                )).map_err(|e| SnatchError::IndexError(format!("Failed to add document: {}", e)))?;
            }
            _ => {}
        }

        Ok(())
    }

    /// Index a session's entries.
    pub fn index_session(&self, session: &Session) -> Result<usize> {
        let entries = session.parse()?;
        let session_id = session.session_id();
        let project = session.project_path();

        let mut writer = self.writer.write();
        let mut indexed = 0;

        for entry in &entries {
            self.index_entry(&mut writer, session_id, project, entry)?;
            indexed += 1;
        }

        Ok(indexed)
    }

    /// Index multiple sessions.
    pub fn index_sessions(&self, sessions: &[Session]) -> Result<IndexingResult> {
        let mut total_indexed = 0;
        let mut session_count = 0;
        let mut errors = Vec::new();

        {
            let mut writer = self.writer.write();

            for session in sessions {
                match session.parse() {
                    Ok(entries) => {
                        let session_id = session.session_id();
                        let project = session.project_path();

                        for entry in &entries {
                            if let Err(e) = self.index_entry(&mut writer, session_id, project, entry)
                            {
                                errors.push((session.session_id().to_string(), e.to_string()));
                            } else {
                                total_indexed += 1;
                            }
                        }
                        session_count += 1;
                    }
                    Err(e) => {
                        errors.push((session.session_id().to_string(), e.to_string()));
                    }
                }
            }
        }

        Ok(IndexingResult {
            documents_indexed: total_indexed,
            sessions_indexed: session_count,
            errors,
        })
    }

    /// Commit pending changes to the index.
    pub fn commit(&self) -> Result<()> {
        let mut writer = self.writer.write();
        writer
            .commit()
            .map_err(|e| SnatchError::IndexError(format!("Failed to commit: {}", e)))?;
        Ok(())
    }

    /// Clear the entire index.
    pub fn clear(&self) -> Result<()> {
        let mut writer = self.writer.write();
        writer
            .delete_all_documents()
            .map_err(|e| SnatchError::IndexError(format!("Failed to clear index: {}", e)))?;
        writer
            .commit()
            .map_err(|e| SnatchError::IndexError(format!("Failed to commit: {}", e)))?;
        Ok(())
    }

    /// Delete documents for a specific session.
    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        let session_id_field = self.schema.get_field(fields::SESSION_ID).expect("schema field");
        let term = tantivy::Term::from_field_text(session_id_field, session_id);

        let writer = self.writer.write();
        writer.delete_term(term);
        Ok(())
    }

    /// Search the index.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let searcher = self.reader.searcher();

        // Parse query - search content by default
        let content_field = self.schema.get_field(fields::CONTENT).expect("schema field");
        let thinking_field = self.schema.get_field(fields::THINKING).expect("schema field");
        let tool_input_field = self.schema.get_field(fields::TOOL_INPUT).expect("schema field");

        let query_parser =
            QueryParser::for_index(&self.index, vec![content_field, thinking_field, tool_input_field]);

        let parsed_query = query_parser.parse_query(query).map_err(|e| {
            SnatchError::InvalidArgument {
                name: "query".to_string(),
                reason: e.to_string(),
            }
        })?;

        let top_docs = searcher
            .search(&parsed_query, &TopDocs::with_limit(limit))
            .map_err(|e| SnatchError::IndexError(format!("Search failed: {}", e)))?;

        let mut results = Vec::new();

        for (score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_address).map_err(|e| {
                SnatchError::IndexError(format!("Failed to retrieve document: {}", e))
            })?;

            let session_id = self.get_text_field(&doc, fields::SESSION_ID);
            let project = self.get_text_field(&doc, fields::PROJECT);
            let uuid = self.get_text_field(&doc, fields::UUID);
            let timestamp = self.get_text_field(&doc, fields::TIMESTAMP);
            let message_type = self.get_text_field(&doc, fields::MESSAGE_TYPE);
            let model = {
                let m = self.get_text_field(&doc, fields::MODEL);
                if m.is_empty() { None } else { Some(m) }
            };
            let content = self.get_text_field(&doc, fields::CONTENT);

            // Create snippet (first 200 chars)
            let content_snippet = if content.len() > 200 {
                format!("{}...", &content[..200])
            } else {
                content
            };

            results.push(SearchHit {
                session_id,
                project,
                uuid,
                timestamp,
                message_type,
                model,
                content_snippet,
                score,
            });
        }

        Ok(results)
    }

    /// Search with advanced options.
    pub fn search_advanced(&self, options: &SearchOptions) -> Result<Vec<SearchHit>> {
        // Build query string with field filters
        let mut query_parts = Vec::new();

        if !options.query.is_empty() {
            query_parts.push(options.query.clone());
        }

        if let Some(ref message_type) = options.message_type {
            query_parts.push(format!("{}:{}", fields::MESSAGE_TYPE, message_type));
        }

        if let Some(ref model) = options.model {
            query_parts.push(format!("{}:{}", fields::MODEL, model));
        }

        if let Some(ref session_id) = options.session_id {
            query_parts.push(format!("{}:{}", fields::SESSION_ID, session_id));
        }

        if let Some(ref tool_name) = options.tool_name {
            query_parts.push(format!("{}:{}", fields::TOOL_NAME, tool_name));
        }

        let full_query = query_parts.join(" AND ");

        self.search(&full_query, options.limit.unwrap_or(DEFAULT_RESULT_LIMIT))
    }

    /// Get index statistics.
    pub fn stats(&self) -> Result<IndexStats> {
        let searcher = self.reader.searcher();
        let document_count = searcher.num_docs();

        // Calculate index size
        let size_bytes = walkdir::WalkDir::new(&self.index_path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
            .sum();

        // Count unique sessions (expensive operation)
        let session_count = 0; // Would require iterating all docs

        Ok(IndexStats {
            document_count,
            session_count,
            size_bytes,
            last_updated: None,
        })
    }

    /// Get text from a field in a document.
    fn get_text_field(&self, doc: &TantivyDocument, field_name: &str) -> String {
        let field = self.schema.get_field(field_name).expect("schema field");
        doc.get_first(field)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    }

    /// Check if the index exists and has documents.
    pub fn is_empty(&self) -> bool {
        self.reader.searcher().num_docs() == 0
    }

    /// Get the index path.
    pub fn path(&self) -> &Path {
        &self.index_path
    }

    /// List all unique tool names in the index with occurrence counts.
    pub fn list_tool_names(&self) -> Result<FieldAggregation> {
        self.aggregate_field(fields::TOOL_NAME)
    }

    /// List all unique models in the index with occurrence counts.
    pub fn list_models(&self) -> Result<FieldAggregation> {
        self.aggregate_field(fields::MODEL)
    }

    /// List all unique message types in the index with occurrence counts.
    pub fn list_message_types(&self) -> Result<FieldAggregation> {
        self.aggregate_field(fields::MESSAGE_TYPE)
    }

    /// List all unique projects in the index with occurrence counts.
    pub fn list_projects(&self) -> Result<FieldAggregation> {
        self.aggregate_field(fields::PROJECT)
    }

    /// Aggregate values for a specific field.
    fn aggregate_field(&self, field_name: &str) -> Result<FieldAggregation> {
        use std::collections::HashMap;
        use tantivy::collector::DocSetCollector;
        use tantivy::query::AllQuery;

        let searcher = self.reader.searcher();
        let field = self.schema.get_field(field_name).map_err(|_| {
            SnatchError::IndexError(format!("Field not found: {}", field_name))
        })?;

        // Collect all documents
        let doc_addresses = searcher
            .search(&AllQuery, &DocSetCollector)
            .map_err(|e| SnatchError::IndexError(format!("Search failed: {}", e)))?;

        // Count field values
        let mut counts: HashMap<String, usize> = HashMap::new();

        for doc_address in doc_addresses {
            let doc: TantivyDocument = searcher.doc(doc_address).map_err(|e| {
                SnatchError::IndexError(format!("Failed to retrieve document: {}", e))
            })?;

            // Get all values for this field (multi-valued fields like tool_name)
            for value in doc.get_all(field) {
                if let Some(text) = value.as_str() {
                    if !text.is_empty() {
                        *counts.entry(text.to_string()).or_insert(0) += 1;
                    }
                }
            }
        }

        // Sort by count descending
        let mut values: Vec<(String, usize)> = counts.into_iter().collect();
        values.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let unique_count = values.len();

        Ok(FieldAggregation {
            field: field_name.to_string(),
            values,
            unique_count,
        })
    }

    /// Search by a specific field value only.
    pub fn search_by_field(&self, field_name: &str, value: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let query_str = format!("{}:{}", field_name, value);
        self.search(&query_str, limit)
    }

    /// Search for messages using a specific tool.
    pub fn search_by_tool(&self, tool_name: &str, limit: usize) -> Result<Vec<SearchHit>> {
        self.search_by_field(fields::TOOL_NAME, tool_name, limit)
    }

    /// Search for messages from a specific model.
    pub fn search_by_model(&self, model: &str, limit: usize) -> Result<Vec<SearchHit>> {
        self.search_by_field(fields::MODEL, model, limit)
    }

    /// Get all tool names that match a prefix (for autocomplete).
    pub fn suggest_tool_names(&self, prefix: &str) -> Result<Vec<String>> {
        let aggregation = self.list_tool_names()?;
        let prefix_lower = prefix.to_lowercase();
        Ok(aggregation
            .values
            .into_iter()
            .filter(|(name, _)| name.to_lowercase().starts_with(&prefix_lower))
            .map(|(name, _)| name)
            .collect())
    }

    /// Get all models that match a prefix (for autocomplete).
    pub fn suggest_models(&self, prefix: &str) -> Result<Vec<String>> {
        let aggregation = self.list_models()?;
        let prefix_lower = prefix.to_lowercase();
        Ok(aggregation
            .values
            .into_iter()
            .filter(|(name, _)| name.to_lowercase().starts_with(&prefix_lower))
            .map(|(name, _)| name)
            .collect())
    }
}

/// Options for advanced search.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    /// The search query.
    pub query: String,
    /// Filter by message type.
    pub message_type: Option<String>,
    /// Filter by model.
    pub model: Option<String>,
    /// Filter by session ID.
    pub session_id: Option<String>,
    /// Filter by tool name.
    pub tool_name: Option<String>,
    /// Include thinking blocks in search.
    pub include_thinking: bool,
    /// Maximum number of results.
    pub limit: Option<usize>,
}

/// Aggregation of field values with counts.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FieldAggregation {
    /// Field name.
    pub field: String,
    /// Values with their occurrence counts, sorted by count descending.
    pub values: Vec<(String, usize)>,
    /// Total unique values.
    pub unique_count: usize,
}

/// Result of an indexing operation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexingResult {
    /// Number of documents indexed.
    pub documents_indexed: usize,
    /// Number of sessions indexed.
    pub sessions_indexed: usize,
    /// Errors encountered.
    pub errors: Vec<(String, String)>,
}

/// Progress update for background indexing.
#[derive(Debug, Clone)]
pub struct IndexingProgress {
    /// Current session being indexed.
    pub current_session: usize,
    /// Total sessions to index.
    pub total_sessions: usize,
    /// Documents indexed so far.
    pub documents_indexed: usize,
    /// Errors encountered so far.
    pub error_count: usize,
    /// Whether indexing is complete.
    pub completed: bool,
    /// Final result (only set when completed).
    pub result: Option<IndexingResult>,
}

impl IndexingProgress {
    /// Get progress as a percentage (0-100).
    pub fn percentage(&self) -> f64 {
        if self.total_sessions == 0 {
            100.0
        } else {
            (self.current_session as f64 / self.total_sessions as f64) * 100.0
        }
    }

    /// Create an in-progress update.
    fn in_progress(current: usize, total: usize, docs: usize, errors: usize) -> Self {
        Self {
            current_session: current,
            total_sessions: total,
            documents_indexed: docs,
            error_count: errors,
            completed: false,
            result: None,
        }
    }

    /// Create a completed update.
    fn complete(result: IndexingResult) -> Self {
        Self {
            current_session: result.sessions_indexed,
            total_sessions: result.sessions_indexed,
            documents_indexed: result.documents_indexed,
            error_count: result.errors.len(),
            completed: true,
            result: Some(result),
        }
    }
}

/// Handle for a background indexing operation.
pub struct BackgroundIndexHandle {
    /// Receiver for progress updates.
    progress_rx: std::sync::mpsc::Receiver<IndexingProgress>,
    /// Handle to the indexing thread.
    thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl BackgroundIndexHandle {
    /// Check if indexing is still running.
    pub fn is_running(&self) -> bool {
        self.thread_handle.as_ref().is_some_and(|h| !h.is_finished())
    }

    /// Get the latest progress (non-blocking).
    pub fn try_progress(&self) -> Option<IndexingProgress> {
        // Drain channel and return latest
        let mut latest = None;
        while let Ok(progress) = self.progress_rx.try_recv() {
            latest = Some(progress);
        }
        latest
    }

    /// Wait for completion and get the final result.
    pub fn wait(mut self) -> Option<IndexingResult> {
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
        // Get final progress
        while let Ok(progress) = self.progress_rx.try_recv() {
            if progress.completed {
                return progress.result;
            }
        }
        None
    }
}

impl SearchIndex {
    /// Start background indexing of sessions.
    ///
    /// Returns a handle that can be used to monitor progress and wait for completion.
    pub fn index_sessions_background(
        index: Arc<SearchIndex>,
        sessions: Vec<crate::discovery::Session>,
    ) -> BackgroundIndexHandle {
        use std::sync::mpsc;
        use std::thread;

        let (progress_tx, progress_rx) = mpsc::channel();

        let handle = thread::spawn(move || {
            let total = sessions.len();
            let mut documents_indexed = 0;
            let mut session_count = 0;
            let mut errors = Vec::new();

            {
                let mut writer = index.writer.write();

                for (i, session) in sessions.iter().enumerate() {
                    match session.parse() {
                        Ok(entries) => {
                            let session_id = session.session_id();
                            let project = session.project_path();

                            for entry in &entries {
                                if let Err(e) = index.index_entry(&mut writer, session_id, project, entry) {
                                    errors.push((session.session_id().to_string(), e.to_string()));
                                } else {
                                    documents_indexed += 1;
                                }
                            }
                            session_count += 1;
                        }
                        Err(e) => {
                            errors.push((session.session_id().to_string(), e.to_string()));
                        }
                    }

                    // Send progress update every 5 sessions or at the end
                    if (i + 1) % 5 == 0 || i + 1 == total {
                        let _ = progress_tx.send(IndexingProgress::in_progress(
                            i + 1,
                            total,
                            documents_indexed,
                            errors.len(),
                        ));
                    }
                }
            }

            // Commit the index
            if let Err(e) = index.commit() {
                errors.push(("commit".to_string(), e.to_string()));
            }

            let result = IndexingResult {
                documents_indexed,
                sessions_indexed: session_count,
                errors,
            };

            let _ = progress_tx.send(IndexingProgress::complete(result));
        });

        BackgroundIndexHandle {
            progress_rx,
            thread_handle: Some(handle),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_create_index() {
        let dir = tempdir().unwrap();
        let index = SearchIndex::open(dir.path().join("test-index")).unwrap();
        assert!(index.is_empty());
    }

    #[test]
    fn test_index_stats() {
        let dir = tempdir().unwrap();
        let index = SearchIndex::open(dir.path().join("test-index")).unwrap();
        let stats = index.stats().unwrap();
        assert_eq!(stats.document_count, 0);
    }

    #[test]
    fn test_search_empty_index() {
        let dir = tempdir().unwrap();
        let index = SearchIndex::open(dir.path().join("test-index")).unwrap();
        let results = index.search("hello", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_clear_index() {
        let dir = tempdir().unwrap();
        let index = SearchIndex::open(dir.path().join("test-index")).unwrap();
        index.clear().unwrap();
        assert!(index.is_empty());
    }

    #[test]
    fn test_list_tool_names_empty() {
        let dir = tempdir().unwrap();
        let index = SearchIndex::open(dir.path().join("test-index")).unwrap();
        let aggregation = index.list_tool_names().unwrap();
        assert_eq!(aggregation.field, "tool_name");
        assert!(aggregation.values.is_empty());
        assert_eq!(aggregation.unique_count, 0);
    }

    #[test]
    fn test_list_models_empty() {
        let dir = tempdir().unwrap();
        let index = SearchIndex::open(dir.path().join("test-index")).unwrap();
        let aggregation = index.list_models().unwrap();
        assert_eq!(aggregation.field, "model");
        assert!(aggregation.values.is_empty());
        assert_eq!(aggregation.unique_count, 0);
    }

    #[test]
    fn test_list_message_types_empty() {
        let dir = tempdir().unwrap();
        let index = SearchIndex::open(dir.path().join("test-index")).unwrap();
        let aggregation = index.list_message_types().unwrap();
        assert_eq!(aggregation.field, "message_type");
        assert!(aggregation.values.is_empty());
    }

    #[test]
    fn test_list_projects_empty() {
        let dir = tempdir().unwrap();
        let index = SearchIndex::open(dir.path().join("test-index")).unwrap();
        let aggregation = index.list_projects().unwrap();
        assert_eq!(aggregation.field, "project");
        assert!(aggregation.values.is_empty());
    }

    #[test]
    fn test_search_by_tool_empty() {
        let dir = tempdir().unwrap();
        let index = SearchIndex::open(dir.path().join("test-index")).unwrap();
        let results = index.search_by_tool("Read", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_by_model_empty() {
        let dir = tempdir().unwrap();
        let index = SearchIndex::open(dir.path().join("test-index")).unwrap();
        let results = index.search_by_model("claude-3", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_suggest_tool_names_empty() {
        let dir = tempdir().unwrap();
        let index = SearchIndex::open(dir.path().join("test-index")).unwrap();
        let suggestions = index.suggest_tool_names("Re").unwrap();
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_suggest_models_empty() {
        let dir = tempdir().unwrap();
        let index = SearchIndex::open(dir.path().join("test-index")).unwrap();
        let suggestions = index.suggest_models("claude").unwrap();
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_field_aggregation_serialization() {
        let aggregation = FieldAggregation {
            field: "tool_name".to_string(),
            values: vec![
                ("Read".to_string(), 50),
                ("Write".to_string(), 30),
                ("Bash".to_string(), 20),
            ],
            unique_count: 3,
        };
        let json = serde_json::to_string(&aggregation).unwrap();
        assert!(json.contains("tool_name"));
        assert!(json.contains("Read"));
        assert!(json.contains("50"));
    }

    #[test]
    fn test_indexing_progress_percentage() {
        let progress = IndexingProgress::in_progress(5, 10, 50, 1);
        assert!((progress.percentage() - 50.0).abs() < 0.01);
        assert!(!progress.completed);
    }

    #[test]
    fn test_indexing_progress_complete() {
        let result = IndexingResult {
            documents_indexed: 100,
            sessions_indexed: 10,
            errors: vec![("err1".to_string(), "error".to_string())],
        };
        let progress = IndexingProgress::complete(result);
        assert!(progress.completed);
        assert_eq!(progress.documents_indexed, 100);
        assert_eq!(progress.error_count, 1);
        assert!(progress.result.is_some());
    }

    #[test]
    fn test_indexing_progress_zero_total() {
        let progress = IndexingProgress::in_progress(0, 0, 0, 0);
        assert!((progress.percentage() - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_background_handle_empty() {
        use std::sync::mpsc;

        let (_tx, rx) = mpsc::channel();
        let handle = BackgroundIndexHandle {
            progress_rx: rx,
            thread_handle: None,
        };

        // No thread, so not running
        assert!(!handle.is_running());

        // No progress available
        assert!(handle.try_progress().is_none());
    }
}
