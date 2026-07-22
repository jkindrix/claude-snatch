//! Session tagging and naming.
//!
//! Provides human-friendly labels for sessions, stored in a JSON file
//! separate from the Claude session data.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Result, SnatchError};
use crate::provider::{LogicalSessionKey, ProviderId, SessionNamespace};
use crate::util::atomic_write;

/// Tag storage filename.
const TAGS_FILENAME: &str = "tags.json";

/// Current tag-store wire format.
const TAG_STORE_VERSION: u32 = 2;

/// Session outcome classification for analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionOutcome {
    /// Session achieved its goal successfully.
    Success,
    /// Session partially achieved its goal.
    Partial,
    /// Session failed to achieve its goal.
    Failed,
    /// Session was abandoned before completion.
    Abandoned,
}

impl std::fmt::Display for SessionOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Success => write!(f, "success"),
            Self::Partial => write!(f, "partial"),
            Self::Failed => write!(f, "failed"),
            Self::Abandoned => write!(f, "abandoned"),
        }
    }
}

impl FromStr for SessionOutcome {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "success" | "s" => Ok(Self::Success),
            "partial" | "p" => Ok(Self::Partial),
            "failed" | "fail" | "f" => Ok(Self::Failed),
            "abandoned" | "abandon" | "a" => Ok(Self::Abandoned),
            _ => Err(format!(
                "Invalid outcome '{}'. Valid values: success, partial, failed, abandoned",
                s
            )),
        }
    }
}

/// A timestamped note/annotation for a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionNote {
    /// The note content.
    pub text: String,
    /// When the note was created.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Optional category/label for the note.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl SessionNote {
    /// Create a new note with the current timestamp.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            created_at: chrono::Utc::now(),
            label: None,
        }
    }

    /// Create a note with a label.
    pub fn with_label(text: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            created_at: chrono::Utc::now(),
            label: Some(label.into()),
        }
    }
}

/// Statistics for session outcomes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutcomeStats {
    /// Number of successful sessions.
    pub success: usize,
    /// Number of partially successful sessions.
    pub partial: usize,
    /// Number of failed sessions.
    pub failed: usize,
    /// Number of abandoned sessions.
    pub abandoned: usize,
    /// Number of sessions without outcome classification.
    pub unclassified: usize,
}

impl OutcomeStats {
    /// Total number of classified sessions.
    pub fn classified(&self) -> usize {
        self.success + self.partial + self.failed + self.abandoned
    }

    /// Success rate as a percentage (success / classified * 100).
    pub fn success_rate(&self) -> f64 {
        let classified = self.classified();
        if classified == 0 {
            0.0
        } else {
            (self.success as f64 / classified as f64) * 100.0
        }
    }
}

/// Session metadata including tags and optional name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMeta {
    /// Human-readable name for the session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Tags associated with the session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Whether this session is bookmarked/favorited.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bookmarked: bool,
    /// Session outcome classification.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<SessionOutcome>,
    /// Notes/annotations for the session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<SessionNote>,
    /// Linked/continuation sessions (qualified logical identities).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked_sessions: Vec<LogicalSessionKey>,
}

impl SessionMeta {
    /// Check if this metadata is empty (no name, tags, bookmark, outcome, notes, or links).
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.tags.is_empty()
            && !self.bookmarked
            && self.outcome.is_none()
            && self.notes.is_empty()
            && self.linked_sessions.is_empty()
    }
}

/// A v2 session metadata record. The key is an object rather than a rendered
/// string so persistent identity never depends on delimiter escaping.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSession {
    key: LogicalSessionKey,
    metadata: SessionMeta,
}

/// A record retained because it could not be interpreted safely.
///
/// Recovery records are written back verbatim inside the v2 envelope. They
/// are never treated as live metadata, but they are also never discarded by a
/// later valid edit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnresolvedTagRecord {
    /// Schema version from which the record came.
    pub source_version: u32,
    /// Why the record was not admitted to the typed store.
    pub reason: String,
    /// Original record payload.
    pub record: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TagStoreV2 {
    version: u32,
    sessions: Vec<StoredSession>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    unresolved: Vec<UnresolvedTagRecord>,
}

/// Tag store for session metadata.
#[derive(Debug, Clone)]
pub struct TagStore {
    /// Version of the tag store format.
    pub version: u32,
    /// Session metadata keyed by structured logical identity.
    pub sessions: BTreeMap<LogicalSessionKey, SessionMeta>,
    /// Records preserved for manual recovery instead of being dropped.
    pub unresolved: Vec<UnresolvedTagRecord>,
    /// Whether this instance was loaded from a pre-v2 store. Not serialized.
    migrated_from_legacy: bool,
}

impl Default for TagStore {
    fn default() -> Self {
        Self {
            version: TAG_STORE_VERSION,
            sessions: BTreeMap::new(),
            unresolved: Vec::new(),
            migrated_from_legacy: false,
        }
    }
}

impl Serialize for TagStore {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.as_v2().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TagStore {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let root = Value::deserialize(deserializer)?;
        let version = store_version(&root).map_err(serde::de::Error::custom)?;
        match version {
            0 | 1 => Self::from_legacy_value(root, version),
            TAG_STORE_VERSION => Self::from_v2_value(root),
            other => Err(SnatchError::InvalidConfig {
                message: format!(
                    "Unsupported tags file version {other}; this build supports through {TAG_STORE_VERSION}"
                ),
            }),
        }
        .map_err(serde::de::Error::custom)
    }
}

/// Legacy metadata differs only in the identity type used for links.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacySessionMeta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    bookmarked: bool,
    #[serde(default)]
    outcome: Option<SessionOutcome>,
    #[serde(default)]
    notes: Vec<SessionNote>,
    #[serde(default)]
    linked_sessions: Vec<String>,
}

impl LegacySessionMeta {
    fn into_v2(self) -> SessionMeta {
        SessionMeta {
            name: self.name,
            tags: self.tags,
            bookmarked: self.bookmarked,
            outcome: self.outcome,
            notes: self.notes,
            linked_sessions: self
                .linked_sessions
                .into_iter()
                .map(|native_id| legacy_key(&native_id))
                .collect(),
        }
    }
}

fn legacy_key(native_id: &str) -> LogicalSessionKey {
    LogicalSessionKey {
        provider: ProviderId::claude_code(),
        namespace: SessionNamespace::global(),
        native_id: native_id.to_string(),
    }
}

fn store_version(root: &Value) -> std::result::Result<u32, String> {
    let Some(raw_version) = root.get("version") else {
        return Ok(0);
    };
    let version = raw_version
        .as_u64()
        .ok_or_else(|| "tags file version must be a non-negative integer".to_string())?;
    u32::try_from(version)
        .map_err(|_| "tags file version is outside the supported range".to_string())
}

fn preserve_legacy_backup(path: &Path) -> Result<()> {
    let original = std::fs::read(path).map_err(|error| {
        SnatchError::io(
            format!("Failed to read legacy tags file: {}", path.display()),
            error,
        )
    })?;
    let stored_version = serde_json::from_slice::<Value>(&original)
        .ok()
        .and_then(|root| root.get("version").and_then(Value::as_u64))
        .unwrap_or(0);
    if stored_version >= u64::from(TAG_STORE_VERSION) {
        return Ok(());
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(TAGS_FILENAME);
    for suffix in 0_u32.. {
        let backup_name = if suffix == 0 {
            format!("{file_name}.v1.bak")
        } else {
            format!("{file_name}.v1.bak.{suffix}")
        };
        let backup = path.with_file_name(backup_name);
        match std::fs::read(&backup) {
            Ok(existing) if existing == original => return Ok(()),
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                atomic_write(&backup, &original)?;
                return Ok(());
            }
            Err(error) => {
                return Err(SnatchError::io(
                    format!("Failed to inspect tags backup: {}", backup.display()),
                    error,
                ));
            }
        }
    }
    unreachable!("u32 backup suffix space exhausted")
}

impl TagStore {
    /// Load tag store from default location.
    pub fn load() -> Result<Self> {
        let path = default_tags_path()?;
        if path.exists() {
            Self::load_from(&path)
        } else {
            Ok(Self::default())
        }
    }

    /// Load tag store from a specific path.
    pub fn load_from(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            SnatchError::io(format!("Failed to read tags file: {}", path.display()), e)
        })?;
        Self::from_json(&content)
    }

    /// Save tag store to default location.
    pub fn save(&self) -> Result<()> {
        let path = default_tags_path()?;
        self.save_to(&path)
    }

    /// Save tag store to a specific path.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if self.migrated_from_legacy && path.exists() {
            preserve_legacy_backup(path)?;
        }

        let content = serde_json::to_string_pretty(&self.as_v2()).map_err(|e| {
            SnatchError::InvalidConfig {
                message: format!("Failed to serialize tags: {e}"),
            }
        })?;

        atomic_write(path, content.as_bytes())?;
        Ok(())
    }

    /// Parse either the structured v2 format or the legacy string-keyed
    /// v0/v1 format. Legacy keys are always provider-native strings; a colon
    /// in one never means that the old record was already qualified.
    fn from_json(content: &str) -> Result<Self> {
        let root: Value =
            serde_json::from_str(content).map_err(|e| SnatchError::InvalidConfig {
                message: format!("Invalid tags file: {e}"),
            })?;
        let version = store_version(&root).map_err(|message| SnatchError::InvalidConfig {
            message: format!("Invalid tags file: {message}"),
        })?;
        match version {
            0 | 1 => Self::from_legacy_value(root, version),
            TAG_STORE_VERSION => Self::from_v2_value(root),
            other => Err(SnatchError::InvalidConfig {
                message: format!(
                    "Unsupported tags file version {other}; this build supports through {TAG_STORE_VERSION}"
                ),
            }),
        }
    }

    fn from_legacy_value(root: Value, version: u32) -> Result<Self> {
        let sessions = root
            .as_object()
            .and_then(|object| object.get("sessions"))
            .and_then(Value::as_object)
            .ok_or_else(|| SnatchError::InvalidConfig {
                message: "Invalid legacy tags file: 'sessions' must be an object".to_string(),
            })?;
        let mut store = Self {
            migrated_from_legacy: true,
            ..Self::default()
        };
        for (field, value) in root
            .as_object()
            .expect("sessions object came from a top-level object")
        {
            if field != "version" && field != "sessions" {
                store.unresolved.push(UnresolvedTagRecord {
                    source_version: version,
                    reason: format!("unknown legacy top-level field '{field}'"),
                    record: serde_json::json!({
                        "top_level_field": field,
                        "value": value,
                    }),
                });
            }
        }
        for (native_id, raw_metadata) in sessions {
            let key = legacy_key(native_id);
            match serde_json::from_value::<LegacySessionMeta>(raw_metadata.clone()) {
                Ok(metadata) => {
                    store.sessions.insert(key, metadata.into_v2());
                }
                Err(error) => store.unresolved.push(UnresolvedTagRecord {
                    source_version: version,
                    reason: format!("invalid metadata for legacy session '{native_id}': {error}"),
                    record: serde_json::json!({
                        "legacy_session_id": native_id,
                        "metadata": raw_metadata,
                    }),
                }),
            }
        }
        Ok(store)
    }

    fn from_v2_value(root: Value) -> Result<Self> {
        let object = root.as_object().ok_or_else(|| SnatchError::InvalidConfig {
            message: "Invalid tags file: top level must be an object".to_string(),
        })?;
        let raw_sessions = object
            .get("sessions")
            .and_then(Value::as_array)
            .ok_or_else(|| SnatchError::InvalidConfig {
                message: "Invalid v2 tags file: 'sessions' must be an array".to_string(),
            })?;
        let mut store = Self::default();
        if let Some(raw_unresolved) = object.get("unresolved") {
            store.unresolved = serde_json::from_value(raw_unresolved.clone()).map_err(|error| {
                SnatchError::InvalidConfig {
                    message: format!("Invalid v2 tags recovery ledger: {error}"),
                }
            })?;
        }
        for (field, value) in object {
            if field != "version" && field != "sessions" && field != "unresolved" {
                store.unresolved.push(UnresolvedTagRecord {
                    source_version: TAG_STORE_VERSION,
                    reason: format!("unknown v2 top-level field '{field}'"),
                    record: serde_json::json!({
                        "top_level_field": field,
                        "value": value,
                    }),
                });
            }
        }

        for raw_record in raw_sessions {
            match serde_json::from_value::<StoredSession>(raw_record.clone()) {
                Ok(record) => {
                    if record.key.provider.0.is_empty()
                        || record.key.namespace.0.is_empty()
                        || record.key.native_id.is_empty()
                    {
                        store.unresolved.push(UnresolvedTagRecord {
                            source_version: TAG_STORE_VERSION,
                            reason: "logical session key contains an empty segment".to_string(),
                            record: raw_record.clone(),
                        });
                    } else {
                        match store.sessions.entry(record.key) {
                            std::collections::btree_map::Entry::Vacant(entry) => {
                                entry.insert(record.metadata);
                            }
                            std::collections::btree_map::Entry::Occupied(entry) => {
                                store.unresolved.push(UnresolvedTagRecord {
                                    source_version: TAG_STORE_VERSION,
                                    reason: format!(
                                        "duplicate logical session key '{}'",
                                        entry.key()
                                    ),
                                    record: raw_record.clone(),
                                });
                            }
                        }
                    }
                }
                Err(error) => store.unresolved.push(UnresolvedTagRecord {
                    source_version: TAG_STORE_VERSION,
                    reason: format!("invalid v2 session record: {error}"),
                    record: raw_record.clone(),
                }),
            }
        }
        Ok(store)
    }

    fn as_v2(&self) -> TagStoreV2 {
        TagStoreV2 {
            version: TAG_STORE_VERSION,
            sessions: self
                .sessions
                .iter()
                .map(|(key, metadata)| StoredSession {
                    key: key.clone(),
                    metadata: metadata.clone(),
                })
                .collect(),
            unresolved: self.unresolved.clone(),
        }
    }

    /// Get metadata for a session.
    pub fn get(&self, session_id: &str) -> Option<&SessionMeta> {
        let key = legacy_key(session_id);
        self.get_key(&key).or_else(|| {
            let mut matches = self.sessions.iter().filter(|(candidate, _)| {
                candidate.provider == key.provider
                    && candidate.namespace == key.namespace
                    && candidate.native_id.starts_with(session_id)
            });
            let first = matches.next().map(|(_, metadata)| metadata);
            if matches.next().is_some() {
                None
            } else {
                first
            }
        })
    }

    /// Get metadata by exact logical identity.
    pub fn get_key(&self, key: &LogicalSessionKey) -> Option<&SessionMeta> {
        self.sessions.get(key)
    }

    /// Stored identities matching a provider/namespace/native-id prefix.
    pub fn matching_keys(
        &self,
        provider: &ProviderId,
        namespace: &SessionNamespace,
        native_prefix: &str,
    ) -> Vec<&LogicalSessionKey> {
        self.sessions
            .keys()
            .filter(|key| {
                key.provider == *provider
                    && key.namespace == *namespace
                    && key.native_id.starts_with(native_prefix)
            })
            .collect()
    }

    /// Get mutable metadata by exact logical identity, creating if needed.
    pub fn get_or_create_key(&mut self, key: &LogicalSessionKey) -> &mut SessionMeta {
        self.sessions.entry(key.clone()).or_default()
    }

    /// Set or update the name for a session.
    pub fn set_name(&mut self, session_id: &str, name: Option<String>) {
        self.set_name_key(&legacy_key(session_id), name);
    }

    /// Set or update the name for a qualified session.
    pub fn set_name_key(&mut self, key: &LogicalSessionKey, name: Option<String>) {
        let meta = self.get_or_create_key(key);
        meta.name = name;
        self.cleanup_empty_key(key);
    }

    /// Add a tag to a session.
    pub fn add_tag(&mut self, session_id: &str, tag: &str) -> bool {
        self.add_tag_key(&legacy_key(session_id), tag)
    }

    /// Add a tag to a qualified session.
    pub fn add_tag_key(&mut self, key: &LogicalSessionKey, tag: &str) -> bool {
        let meta = self.get_or_create_key(key);
        let tag = normalize_tag(tag);
        if meta.tags.contains(&tag) {
            false
        } else {
            meta.tags.push(tag);
            meta.tags.sort();
            true
        }
    }

    /// Remove a tag from a session.
    pub fn remove_tag(&mut self, session_id: &str, tag: &str) -> bool {
        let Some(key) = self.resolve_id(session_id).cloned() else {
            return false;
        };
        self.remove_tag_key(&key, tag)
    }

    /// Remove a tag from a qualified session.
    pub fn remove_tag_key(&mut self, key: &LogicalSessionKey, tag: &str) -> bool {
        let tag = normalize_tag(tag);
        if let Some(meta) = self.sessions.get_mut(key) {
            if let Some(pos) = meta.tags.iter().position(|existing| existing == &tag) {
                meta.tags.remove(pos);
                self.cleanup_empty_key(key);
                return true;
            }
        }
        false
    }

    /// Set bookmark status.
    pub fn set_bookmark(&mut self, session_id: &str, bookmarked: bool) {
        self.set_bookmark_key(&legacy_key(session_id), bookmarked);
    }

    /// Set bookmark status for a qualified session.
    pub fn set_bookmark_key(&mut self, key: &LogicalSessionKey, bookmarked: bool) {
        let meta = self.get_or_create_key(key);
        meta.bookmarked = bookmarked;
        self.cleanup_empty_key(key);
    }

    /// Get all bookmarked session keys.
    pub fn bookmarked_sessions(&self) -> Vec<&LogicalSessionKey> {
        self.sessions
            .iter()
            .filter(|(_, m)| m.bookmarked)
            .map(|(key, _)| key)
            .collect()
    }

    /// Set outcome classification for a session.
    pub fn set_outcome(&mut self, session_id: &str, outcome: Option<SessionOutcome>) {
        self.set_outcome_key(&legacy_key(session_id), outcome);
    }

    /// Set outcome classification for a qualified session.
    pub fn set_outcome_key(&mut self, key: &LogicalSessionKey, outcome: Option<SessionOutcome>) {
        let meta = self.get_or_create_key(key);
        meta.outcome = outcome;
        self.cleanup_empty_key(key);
    }

    /// Get all sessions with a specific outcome.
    pub fn sessions_with_outcome(&self, outcome: SessionOutcome) -> Vec<&LogicalSessionKey> {
        self.sessions
            .iter()
            .filter(|(_, m)| m.outcome == Some(outcome))
            .map(|(key, _)| key)
            .collect()
    }

    /// Get outcome statistics across all sessions.
    pub fn outcome_stats(&self) -> OutcomeStats {
        let mut stats = OutcomeStats::default();
        for meta in self.sessions.values() {
            match meta.outcome {
                Some(SessionOutcome::Success) => stats.success += 1,
                Some(SessionOutcome::Partial) => stats.partial += 1,
                Some(SessionOutcome::Failed) => stats.failed += 1,
                Some(SessionOutcome::Abandoned) => stats.abandoned += 1,
                None => stats.unclassified += 1,
            }
        }
        stats
    }

    /// Add a note to a session.
    pub fn add_note(&mut self, session_id: &str, text: &str, label: Option<&str>) {
        self.add_note_key(&legacy_key(session_id), text, label);
    }

    /// Add a note to a qualified session.
    pub fn add_note_key(&mut self, key: &LogicalSessionKey, text: &str, label: Option<&str>) {
        let meta = self.get_or_create_key(key);
        let note = if let Some(label) = label {
            SessionNote::with_label(text, label)
        } else {
            SessionNote::new(text)
        };
        meta.notes.push(note);
    }

    /// Remove a note from a session by index.
    pub fn remove_note(&mut self, session_id: &str, index: usize) -> bool {
        let Some(key) = self.resolve_id(session_id).cloned() else {
            return false;
        };
        self.remove_note_key(&key, index)
    }

    /// Remove a note from a qualified session by index.
    pub fn remove_note_key(&mut self, key: &LogicalSessionKey, index: usize) -> bool {
        if let Some(meta) = self.sessions.get_mut(key) {
            if index < meta.notes.len() {
                meta.notes.remove(index);
                self.cleanup_empty_key(key);
                return true;
            }
        }
        false
    }

    /// Clear all notes for a session.
    pub fn clear_notes(&mut self, session_id: &str) {
        if let Some(key) = self.resolve_id(session_id).cloned() {
            self.clear_notes_key(&key);
        }
    }

    /// Clear all notes for a qualified session.
    pub fn clear_notes_key(&mut self, key: &LogicalSessionKey) {
        if let Some(meta) = self.sessions.get_mut(key) {
            meta.notes.clear();
            self.cleanup_empty_key(key);
        }
    }

    /// Get notes for a session.
    pub fn get_notes(&self, session_id: &str) -> Option<&[SessionNote]> {
        self.get(session_id).map(|m| m.notes.as_slice())
    }

    /// Get notes for a qualified session.
    pub fn get_notes_key(&self, key: &LogicalSessionKey) -> Option<&[SessionNote]> {
        self.get_key(key).map(|metadata| metadata.notes.as_slice())
    }

    /// Get all sessions with notes.
    pub fn sessions_with_notes(&self) -> Vec<&LogicalSessionKey> {
        self.sessions
            .iter()
            .filter(|(_, m)| !m.notes.is_empty())
            .map(|(key, _)| key)
            .collect()
    }

    /// Count total notes across all sessions.
    pub fn note_count(&self) -> usize {
        self.sessions.values().map(|m| m.notes.len()).sum()
    }

    /// Get all sessions with a specific tag.
    pub fn sessions_with_tag(&self, tag: &str) -> Vec<&LogicalSessionKey> {
        let tag = normalize_tag(tag);
        self.sessions
            .iter()
            .filter(|(_, m)| m.tags.contains(&tag))
            .map(|(key, _)| key)
            .collect()
    }

    /// Get all unique tags.
    pub fn all_tags(&self) -> Vec<&str> {
        let mut tags: Vec<_> = self
            .sessions
            .values()
            .flat_map(|m| m.tags.iter().map(|s| s.as_str()))
            .collect();
        tags.sort_unstable();
        tags.dedup();
        tags
    }

    /// Remove entry if it has no useful metadata.
    fn cleanup_empty_key(&mut self, key: &LogicalSessionKey) {
        if let Some(meta) = self.sessions.get(key) {
            if meta.is_empty() {
                self.sessions.remove(key);
            }
        }
    }

    /// Resolve a short session ID to a full ID if it exists in the store.
    pub fn resolve_id<'a>(&'a self, short_id: &str) -> Option<&'a LogicalSessionKey> {
        let exact = legacy_key(short_id);
        if let Some((key, _)) = self.sessions.get_key_value(&exact) {
            return Some(key);
        }
        let mut matches = self.sessions.keys().filter(|key| {
            key.provider == ProviderId::claude_code()
                && key.namespace == SessionNamespace::global()
                && key.native_id.starts_with(short_id)
        });
        let first = matches.next();
        if matches.next().is_some() {
            None
        } else {
            first
        }
    }

    /// Link two sessions together (bidirectional relationship).
    /// Returns true if the link was created, false if it already existed.
    pub fn link_sessions(&mut self, session_a: &str, session_b: &str) -> bool {
        self.link_session_keys(&legacy_key(session_a), &legacy_key(session_b))
    }

    /// Link two qualified sessions together (bidirectional relationship).
    pub fn link_session_keys(
        &mut self,
        session_a: &LogicalSessionKey,
        session_b: &LogicalSessionKey,
    ) -> bool {
        // Add B to A's links
        let meta_a = self.get_or_create_key(session_a);
        let already_linked_a = meta_a.linked_sessions.contains(session_b);
        if !already_linked_a {
            meta_a.linked_sessions.push(session_b.clone());
            meta_a.linked_sessions.sort();
        }

        // Add A to B's links
        let meta_b = self.get_or_create_key(session_b);
        let already_linked_b = meta_b.linked_sessions.contains(session_a);
        if !already_linked_b {
            meta_b.linked_sessions.push(session_a.clone());
            meta_b.linked_sessions.sort();
        }

        // Return true if either link was new
        !already_linked_a || !already_linked_b
    }

    /// Unlink two sessions (bidirectional).
    /// Returns true if a link was removed, false if they weren't linked.
    pub fn unlink_sessions(&mut self, session_a: &str, session_b: &str) -> bool {
        self.unlink_session_keys(&legacy_key(session_a), &legacy_key(session_b))
    }

    /// Unlink two qualified sessions.
    pub fn unlink_session_keys(
        &mut self,
        session_a: &LogicalSessionKey,
        session_b: &LogicalSessionKey,
    ) -> bool {
        let mut removed = false;

        // Remove B from A's links
        if let Some(meta_a) = self.sessions.get_mut(session_a) {
            if let Some(pos) = meta_a.linked_sessions.iter().position(|s| s == session_b) {
                meta_a.linked_sessions.remove(pos);
                removed = true;
            }
        }

        // Remove A from B's links
        if let Some(meta_b) = self.sessions.get_mut(session_b) {
            if let Some(pos) = meta_b.linked_sessions.iter().position(|s| s == session_a) {
                meta_b.linked_sessions.remove(pos);
                removed = true;
            }
        }

        // Cleanup empty entries
        self.cleanup_empty_key(session_a);
        self.cleanup_empty_key(session_b);

        removed
    }

    /// Get all sessions linked to a given session.
    pub fn get_linked_sessions(&self, session_id: &str) -> Vec<&LogicalSessionKey> {
        self.get(session_id)
            .map(|m| m.linked_sessions.iter().collect())
            .unwrap_or_default()
    }

    /// Get all sessions linked to a qualified session.
    pub fn get_linked_session_keys(&self, key: &LogicalSessionKey) -> Vec<&LogicalSessionKey> {
        self.get_key(key)
            .map(|metadata| metadata.linked_sessions.iter().collect())
            .unwrap_or_default()
    }

    /// Get all sessions that have any links.
    pub fn sessions_with_links(&self) -> Vec<&LogicalSessionKey> {
        self.sessions
            .iter()
            .filter(|(_, m)| !m.linked_sessions.is_empty())
            .map(|(key, _)| key)
            .collect()
    }
}

/// Normalize a tag to lowercase with hyphens.
fn normalize_tag(tag: &str) -> String {
    tag.trim()
        .to_lowercase()
        .replace(' ', "-")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// Get the default tags storage path.
///
/// `SNATCH_CONFIG_DIR` overrides the OS config directory. `dirs::config_dir()`
/// honors `XDG_CONFIG_HOME` only on Linux (macOS/Windows use native locations),
/// so a portable, explicit override is needed for tools and cross-platform
/// tests — mirroring how `SNATCH_CLAUDE_DIR` overrides the data directory.
pub fn default_tags_path() -> Result<PathBuf> {
    let config_dir = match std::env::var_os("SNATCH_CONFIG_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => dirs::config_dir().ok_or_else(|| SnatchError::Unsupported {
            feature: "config directory discovery".to_string(),
        })?,
    };

    Ok(config_dir.join("claude-snatch").join(TAGS_FILENAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contains_native(keys: &[&LogicalSessionKey], native_id: &str) -> bool {
        keys.iter().any(|key| key.native_id == native_id)
    }

    #[test]
    fn test_normalize_tag() {
        assert_eq!(normalize_tag("API Refactor"), "api-refactor");
        assert_eq!(normalize_tag("bug_fix"), "bug_fix");
        assert_eq!(normalize_tag("  TEST  "), "test");
        assert_eq!(normalize_tag("special!@#chars"), "specialchars");
    }

    #[test]
    fn test_session_meta_is_empty() {
        let meta = SessionMeta::default();
        assert!(meta.is_empty());

        let meta = SessionMeta {
            name: Some("test".to_string()),
            ..Default::default()
        };
        assert!(!meta.is_empty());

        let meta = SessionMeta {
            tags: vec!["tag".to_string()],
            ..Default::default()
        };
        assert!(!meta.is_empty());

        let meta = SessionMeta {
            bookmarked: true,
            ..Default::default()
        };
        assert!(!meta.is_empty());
    }

    #[test]
    fn test_tag_store_add_remove() {
        let mut store = TagStore::default();
        let session_id = "test-session-id";

        // Add tag
        assert!(store.add_tag(session_id, "feature"));
        assert!(!store.add_tag(session_id, "feature")); // Duplicate

        // Check tag exists
        let meta = store.get(session_id).unwrap();
        assert!(meta.tags.contains(&"feature".to_string()));

        // Remove tag
        assert!(store.remove_tag(session_id, "feature"));
        assert!(!store.remove_tag(session_id, "nonexistent"));

        // Entry should be cleaned up
        assert!(store.get(session_id).is_none());
    }

    #[test]
    fn test_tag_store_name() {
        let mut store = TagStore::default();
        let session_id = "test-session";

        store.set_name(session_id, Some("My Session".to_string()));
        assert_eq!(
            store.get(session_id).unwrap().name,
            Some("My Session".to_string())
        );

        store.set_name(session_id, None);
        assert!(store.get(session_id).is_none());
    }

    #[test]
    fn test_tag_store_bookmark() {
        let mut store = TagStore::default();
        let session_id = "test-session";

        store.set_bookmark(session_id, true);
        assert!(store.get(session_id).unwrap().bookmarked);

        let bookmarked = store.bookmarked_sessions();
        assert!(contains_native(&bookmarked, session_id));

        store.set_bookmark(session_id, false);
        assert!(store.get(session_id).is_none());
    }

    #[test]
    fn test_all_tags() {
        let mut store = TagStore::default();

        store.add_tag("session1", "feature");
        store.add_tag("session1", "urgent");
        store.add_tag("session2", "feature");
        store.add_tag("session2", "bug");

        let tags = store.all_tags();
        assert_eq!(tags, vec!["bug", "feature", "urgent"]);
    }

    #[test]
    fn test_sessions_with_tag() {
        let mut store = TagStore::default();

        store.add_tag("session1", "feature");
        store.add_tag("session2", "feature");
        store.add_tag("session3", "bug");

        let sessions = store.sessions_with_tag("feature");
        assert_eq!(sessions.len(), 2);
        assert!(contains_native(&sessions, "session1"));
        assert!(contains_native(&sessions, "session2"));
    }

    #[test]
    fn test_short_id_resolution() {
        let mut store = TagStore::default();
        let full_id = "40afc8a7-3fcb-4d29-b1ee-100b81b8c6c0";

        store.add_tag(full_id, "test");

        // Full ID lookup
        assert!(store.get(full_id).is_some());

        // Short ID lookup
        assert!(store.get("40afc8a7").is_some());
        assert!(store.get("40afc").is_some());
    }

    #[test]
    fn test_serialization() {
        let mut store = TagStore::default();
        store.add_tag("session1", "feature");
        store.set_name("session1", Some("My Session".to_string()));
        store.set_bookmark("session2", true);

        let json = serde_json::to_string(&store).unwrap();
        let loaded: TagStore = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.sessions.len(), 2);
        assert!(loaded
            .get("session1")
            .unwrap()
            .tags
            .contains(&"feature".to_string()));
        assert!(loaded.get("session2").unwrap().bookmarked);
    }

    #[test]
    fn test_session_outcome() {
        let mut store = TagStore::default();
        let session_id = "test-session";

        // Set outcome
        store.set_outcome(session_id, Some(SessionOutcome::Success));
        assert_eq!(
            store.get(session_id).unwrap().outcome,
            Some(SessionOutcome::Success)
        );

        // Change outcome
        store.set_outcome(session_id, Some(SessionOutcome::Failed));
        assert_eq!(
            store.get(session_id).unwrap().outcome,
            Some(SessionOutcome::Failed)
        );

        // Clear outcome
        store.set_outcome(session_id, None);
        assert!(store.get(session_id).is_none()); // Entry should be cleaned up
    }

    #[test]
    fn test_sessions_with_outcome() {
        let mut store = TagStore::default();

        store.set_outcome("session1", Some(SessionOutcome::Success));
        store.set_outcome("session2", Some(SessionOutcome::Success));
        store.set_outcome("session3", Some(SessionOutcome::Failed));
        store.set_outcome("session4", Some(SessionOutcome::Partial));

        let successful = store.sessions_with_outcome(SessionOutcome::Success);
        assert_eq!(successful.len(), 2);
        assert!(contains_native(&successful, "session1"));
        assert!(contains_native(&successful, "session2"));

        let failed = store.sessions_with_outcome(SessionOutcome::Failed);
        assert_eq!(failed.len(), 1);
        assert!(contains_native(&failed, "session3"));
    }

    #[test]
    fn test_outcome_stats() {
        let mut store = TagStore::default();

        store.set_outcome("s1", Some(SessionOutcome::Success));
        store.set_outcome("s2", Some(SessionOutcome::Success));
        store.set_outcome("s3", Some(SessionOutcome::Success));
        store.set_outcome("s4", Some(SessionOutcome::Partial));
        store.set_outcome("s5", Some(SessionOutcome::Failed));
        store.add_tag("s6", "unclassified"); // No outcome

        let stats = store.outcome_stats();
        assert_eq!(stats.success, 3);
        assert_eq!(stats.partial, 1);
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.abandoned, 0);
        assert_eq!(stats.unclassified, 1);
        assert_eq!(stats.classified(), 5);
        assert!((stats.success_rate() - 60.0).abs() < 0.01);
    }

    #[test]
    fn test_outcome_parse() {
        assert_eq!(
            "success".parse::<SessionOutcome>().unwrap(),
            SessionOutcome::Success
        );
        assert_eq!(
            "s".parse::<SessionOutcome>().unwrap(),
            SessionOutcome::Success
        );
        assert_eq!(
            "partial".parse::<SessionOutcome>().unwrap(),
            SessionOutcome::Partial
        );
        assert_eq!(
            "p".parse::<SessionOutcome>().unwrap(),
            SessionOutcome::Partial
        );
        assert_eq!(
            "failed".parse::<SessionOutcome>().unwrap(),
            SessionOutcome::Failed
        );
        assert_eq!(
            "fail".parse::<SessionOutcome>().unwrap(),
            SessionOutcome::Failed
        );
        assert_eq!(
            "f".parse::<SessionOutcome>().unwrap(),
            SessionOutcome::Failed
        );
        assert_eq!(
            "abandoned".parse::<SessionOutcome>().unwrap(),
            SessionOutcome::Abandoned
        );
        assert_eq!(
            "a".parse::<SessionOutcome>().unwrap(),
            SessionOutcome::Abandoned
        );
        assert!("invalid".parse::<SessionOutcome>().is_err());
    }

    #[test]
    fn test_outcome_serialization() {
        let mut store = TagStore::default();
        store.set_outcome("session1", Some(SessionOutcome::Success));
        store.set_name("session1", Some("Test".to_string()));

        let json = serde_json::to_string(&store).unwrap();
        let loaded: TagStore = serde_json::from_str(&json).unwrap();

        assert_eq!(
            loaded.get("session1").unwrap().outcome,
            Some(SessionOutcome::Success)
        );
    }

    #[test]
    fn test_session_note_new() {
        let note = SessionNote::new("Test note");
        assert_eq!(note.text, "Test note");
        assert!(note.label.is_none());
    }

    #[test]
    fn test_session_note_with_label() {
        let note = SessionNote::with_label("Test note", "todo");
        assert_eq!(note.text, "Test note");
        assert_eq!(note.label, Some("todo".to_string()));
    }

    #[test]
    fn test_add_note() {
        let mut store = TagStore::default();
        let session_id = "test-session";

        store.add_note(session_id, "First note", None);
        store.add_note(session_id, "Second note", Some("important"));

        let notes = store.get_notes(session_id).unwrap();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].text, "First note");
        assert!(notes[0].label.is_none());
        assert_eq!(notes[1].text, "Second note");
        assert_eq!(notes[1].label, Some("important".to_string()));
    }

    #[test]
    fn test_remove_note() {
        let mut store = TagStore::default();
        let session_id = "test-session";

        store.add_note(session_id, "First", None);
        store.add_note(session_id, "Second", None);
        store.add_note(session_id, "Third", None);

        // Remove middle note
        assert!(store.remove_note(session_id, 1));
        let notes = store.get_notes(session_id).unwrap();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].text, "First");
        assert_eq!(notes[1].text, "Third");

        // Invalid index
        assert!(!store.remove_note(session_id, 10));
    }

    #[test]
    fn test_clear_notes() {
        let mut store = TagStore::default();
        let session_id = "test-session";

        store.add_note(session_id, "Note 1", None);
        store.add_note(session_id, "Note 2", None);

        store.clear_notes(session_id);
        assert!(store.get(session_id).is_none()); // Entry should be cleaned up
    }

    #[test]
    fn test_sessions_with_notes() {
        let mut store = TagStore::default();

        store.add_note("session1", "Note", None);
        store.add_note("session2", "Note", None);
        store.add_tag("session3", "no-notes");

        let sessions = store.sessions_with_notes();
        assert_eq!(sessions.len(), 2);
        assert!(contains_native(&sessions, "session1"));
        assert!(contains_native(&sessions, "session2"));
    }

    #[test]
    fn test_note_count() {
        let mut store = TagStore::default();

        store.add_note("session1", "Note 1", None);
        store.add_note("session1", "Note 2", None);
        store.add_note("session2", "Note 3", None);

        assert_eq!(store.note_count(), 3);
    }

    #[test]
    fn test_notes_serialization() {
        let mut store = TagStore::default();
        store.add_note("session1", "Test note", Some("todo"));

        let json = serde_json::to_string(&store).unwrap();
        let loaded: TagStore = serde_json::from_str(&json).unwrap();

        let notes = loaded.get_notes("session1").unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].text, "Test note");
        assert_eq!(notes[0].label, Some("todo".to_string()));
    }

    #[test]
    fn test_session_meta_is_empty_with_notes() {
        let meta = SessionMeta {
            notes: vec![SessionNote::new("test")],
            ..Default::default()
        };
        assert!(!meta.is_empty());
    }

    #[test]
    fn test_session_meta_is_empty_with_links() {
        let meta = SessionMeta {
            linked_sessions: vec![legacy_key("other-session")],
            ..Default::default()
        };
        assert!(!meta.is_empty());
    }

    #[test]
    fn test_link_sessions() {
        let mut store = TagStore::default();

        // Link two sessions
        assert!(store.link_sessions("session1", "session2"));

        // Check both directions
        let linked1 = store.get_linked_sessions("session1");
        assert!(contains_native(&linked1, "session2"));

        let linked2 = store.get_linked_sessions("session2");
        assert!(contains_native(&linked2, "session1"));
    }

    #[test]
    fn test_link_sessions_already_linked() {
        let mut store = TagStore::default();

        // First link
        assert!(store.link_sessions("session1", "session2"));

        // Second link should return false (already linked)
        assert!(!store.link_sessions("session1", "session2"));
    }

    #[test]
    fn test_unlink_sessions() {
        let mut store = TagStore::default();

        // Link then unlink
        store.link_sessions("session1", "session2");
        assert!(store.unlink_sessions("session1", "session2"));

        // Both should have no links
        assert!(store.get_linked_sessions("session1").is_empty());
        assert!(store.get_linked_sessions("session2").is_empty());

        // Entries should be cleaned up
        assert!(store.get("session1").is_none());
        assert!(store.get("session2").is_none());
    }

    #[test]
    fn test_unlink_sessions_not_linked() {
        let mut store = TagStore::default();

        // Try to unlink sessions that aren't linked
        assert!(!store.unlink_sessions("session1", "session2"));
    }

    #[test]
    fn test_sessions_with_links() {
        let mut store = TagStore::default();

        store.link_sessions("session1", "session2");
        store.link_sessions("session1", "session3");
        store.add_tag("session4", "no-links");

        let with_links = store.sessions_with_links();
        assert_eq!(with_links.len(), 3); // session1, session2, session3
        assert!(contains_native(&with_links, "session1"));
        assert!(contains_native(&with_links, "session2"));
        assert!(contains_native(&with_links, "session3"));
        assert!(!contains_native(&with_links, "session4"));
    }

    #[test]
    fn test_link_serialization() {
        let mut store = TagStore::default();
        store.link_sessions("session1", "session2");
        store.set_name("session1", Some("Test".to_string()));

        let json = serde_json::to_string(&store).unwrap();
        let loaded: TagStore = serde_json::from_str(&json).unwrap();

        let linked = loaded.get_linked_sessions("session1");
        assert!(contains_native(&linked, "session2"));
    }

    #[test]
    fn legacy_migration_treats_colons_as_native_and_migrates_links() {
        let store = TagStore::from_json(
            r#"{
                "version": 1,
                "sessions": {
                    "codex:not-qualified": {
                        "name": "literal legacy id",
                        "linked_sessions": ["other:literal"]
                    }
                }
            }"#,
        )
        .unwrap();

        let key = legacy_key("codex:not-qualified");
        let metadata = store.get_key(&key).unwrap();
        assert_eq!(metadata.name.as_deref(), Some("literal legacy id"));
        assert_eq!(metadata.linked_sessions, vec![legacy_key("other:literal")]);
        assert!(store.migrated_from_legacy);
        assert_eq!(store.version, TAG_STORE_VERSION);
    }

    #[test]
    fn legacy_save_is_structured_reversible_and_backed_up_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(TAGS_FILENAME);
        let legacy = br#"{"version":1,"sessions":{"session-a":{"tags":["keep"]}}}"#;
        std::fs::write(&path, legacy).unwrap();

        let mut store = TagStore::load_from(&path).unwrap();
        store.set_bookmark("session-a", true);
        store.save_to(&path).unwrap();

        assert_eq!(
            std::fs::read(path.with_file_name("tags.json.v1.bak")).unwrap(),
            legacy
        );
        let wire: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(wire["version"], TAG_STORE_VERSION);
        assert!(wire["sessions"].is_array());
        assert_eq!(wire["sessions"][0]["key"]["provider"], "claude-code");
        assert_eq!(wire["sessions"][0]["key"]["namespace"], "global");
        assert_eq!(wire["sessions"][0]["key"]["native_id"], "session-a");

        // Saving the same migrated in-memory value again must not back up the
        // already-migrated v2 file as though it were another legacy source.
        store.save_to(&path).unwrap();
        assert!(!path.with_file_name("tags.json.v1.bak.1").exists());

        let loaded = TagStore::load_from(&path).unwrap();
        let metadata = loaded.get("session-a").unwrap();
        assert_eq!(metadata.tags, vec!["keep"]);
        assert!(metadata.bookmarked);
    }

    #[test]
    fn malformed_and_duplicate_records_survive_in_recovery_ledger() {
        let valid = serde_json::json!({
            "key": {
                "provider": "claude-code",
                "namespace": "global",
                "native_id": "same"
            },
            "metadata": {"tags": ["valid"]}
        });
        let duplicate = serde_json::json!({
            "key": {
                "provider": "claude-code",
                "namespace": "global",
                "native_id": "same"
            },
            "metadata": {"tags": ["must-not-overwrite"]}
        });
        let malformed = serde_json::json!({
            "key": {
                "provider": "",
                "namespace": "global",
                "native_id": "broken"
            },
            "metadata": {"bookmarked": true}
        });
        let root = serde_json::json!({
            "version": TAG_STORE_VERSION,
            "sessions": [valid, duplicate.clone(), malformed.clone()]
        });

        let store = TagStore::from_json(&root.to_string()).unwrap();
        assert_eq!(store.sessions.len(), 1);
        assert_eq!(store.unresolved.len(), 2);
        assert_eq!(store.unresolved[0].record, duplicate);
        assert_eq!(store.unresolved[1].record, malformed);

        let encoded = serde_json::to_string(&store).unwrap();
        let reloaded = TagStore::from_json(&encoded).unwrap();
        assert_eq!(reloaded.sessions.len(), 1);
        assert_eq!(reloaded.unresolved.len(), 2);
    }

    #[test]
    fn malformed_legacy_metadata_is_preserved_not_dropped() {
        let store = TagStore::from_json(
            r#"{
                "version": 1,
                "sessions": {
                    "good": {"tags": ["keep"]},
                    "bad": {"outcome": "future-value", "notes": [42]}
                }
            }"#,
        )
        .unwrap();
        assert!(store.get("good").is_some());
        assert!(store.get("bad").is_none());
        assert_eq!(store.unresolved.len(), 1);
        assert_eq!(store.unresolved[0].record["legacy_session_id"], "bad");
    }

    #[test]
    fn unknown_legacy_metadata_field_quarantines_the_complete_record() {
        let store = TagStore::from_json(
            r#"{
                "version": 1,
                "sessions": {
                    "future": {"tags": ["keep"], "future_field": {"nested": true}}
                }
            }"#,
        )
        .unwrap();
        assert!(store.sessions.is_empty());
        assert_eq!(store.unresolved.len(), 1);
        assert_eq!(
            store.unresolved[0].record["metadata"]["future_field"]["nested"],
            true
        );
    }

    #[test]
    fn identical_native_ids_across_providers_and_namespaces_do_not_alias() {
        let mut store = TagStore::default();
        let claude = legacy_key("same");
        let other_provider = LogicalSessionKey {
            provider: ProviderId("other-provider".to_string()),
            namespace: SessionNamespace::global(),
            native_id: "same".to_string(),
        };
        let other_namespace = LogicalSessionKey {
            provider: ProviderId::claude_code(),
            namespace: SessionNamespace("imported".to_string()),
            native_id: "same".to_string(),
        };
        store.add_tag_key(&claude, "claude");
        store.add_tag_key(&other_provider, "provider");
        store.add_tag_key(&other_namespace, "namespace");

        assert_eq!(store.sessions.len(), 3);
        assert_eq!(store.get_key(&claude).unwrap().tags, vec!["claude"]);
        assert_eq!(
            store.get_key(&other_provider).unwrap().tags,
            vec!["provider"]
        );
        assert_eq!(
            store.get_key(&other_namespace).unwrap().tags,
            vec!["namespace"]
        );
    }

    #[test]
    fn ambiguous_legacy_prefix_does_not_choose_by_map_order() {
        let mut store = TagStore::default();
        store.add_tag("same-one", "one");
        store.add_tag("same-two", "two");
        assert!(store.get("same").is_none());
        assert!(store.resolve_id("same").is_none());
    }

    #[test]
    fn future_store_version_is_refused_without_downgrade() {
        let error = TagStore::from_json(r#"{"version":99,"sessions":[]}"#).unwrap_err();
        assert!(error
            .to_string()
            .contains("Unsupported tags file version 99"));
    }

    #[test]
    fn malformed_version_is_not_reinterpreted_as_legacy() {
        let error = TagStore::from_json(r#"{"version":"2","sessions":{}}"#).unwrap_err();
        assert!(error
            .to_string()
            .contains("version must be a non-negative integer"));
    }
}
