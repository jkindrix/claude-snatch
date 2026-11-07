//! Schema-drift diagnostics: what in the session logs does snatch not model?
//!
//! Claude Code's on-disk schema drifts (new entry types, attachment kinds,
//! system subtypes; fields moving or emptying out), and snatch's tolerant
//! parser absorbs the drift silently — data lands in `Unknown`/`Other`/`extra`
//! and features degrade without a test failing. The doctor makes that drift
//! visible: it aggregates everything unmodeled (with counts and last-seen
//! dates, so fossils are distinguishable from live features) plus the known
//! degradation signals (empty thinking text, unparsed/salvaged lines,
//! unpriced models).

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use serde::Serialize;

use crate::model::message::LogEntry;
use crate::model::{ContentBlock, ModelPricing, SystemSubtype, UserContent};
use crate::parser::ParseStats;

/// Attachment kinds whose payload is always rendered in conversation outputs.
/// `queued_command` renders conditionally (human prompts only) and is handled
/// per entry; every other kind is marker-only. Keep in sync with
/// `analysis::extraction::render_attachment_content`.
const RENDERED_ATTACHMENT_KINDS: [&str; 2] = ["file", "edited_text_file"];

/// One unmodeled thing observed in the corpus.
#[derive(Debug, Clone, Serialize)]
pub struct DriftSighting {
    /// Total occurrences.
    pub count: usize,
    /// Distinct sessions it appeared in.
    pub session_count: usize,
    /// Most recent timestamp observed (fossil vs live signal).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<DateTime<Utc>>,
    /// One session id it appears in, for follow-up digging.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example_session: Option<String>,
}

/// An attachment kind observed in the corpus.
#[derive(Debug, Clone, Serialize)]
pub struct AttachmentSighting {
    /// Occurrences whose payload conversation outputs would render; the rest
    /// are marker-only. Rendering can be conditional per entry (e.g.
    /// `queued_command` renders human prompts but not task notifications).
    pub rendered_count: usize,
    /// Occurrence counts and recency.
    #[serde(flatten)]
    pub sighting: DriftSighting,
}

/// One parse problem, grouped by its normalized error message.
#[derive(Debug, Clone, Serialize)]
pub struct ParseIssueSighting {
    /// Total occurrences.
    pub count: usize,
    /// Distinct sessions it appeared in.
    pub session_count: usize,
    /// One `<session id>:<line>` it appears at, for follow-up digging.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<String>,
}

#[derive(Debug, Default)]
struct IssueBuilder {
    count: usize,
    sessions: HashSet<String>,
    example: Option<String>,
}

impl IssueBuilder {
    fn record(&mut self, session_id: &str, line: usize) {
        self.count += 1;
        self.sessions.insert(session_id.to_string());
        if self.example.is_none() {
            self.example = Some(format!("{session_id}:{line}"));
        }
    }

    fn finish(self) -> ParseIssueSighting {
        ParseIssueSighting {
            count: self.count,
            session_count: self.sessions.len(),
            example: self.example,
        }
    }
}

/// Reduce a parse error to its cause so identical failures group together:
/// drops the "Failed to parse JSONL at line N: " wrapper, serde's
/// " at line N column M" position, and the torn-line salvage note.
fn normalize_parse_error(message: &str) -> String {
    let mut msg = message;
    if let Some(rest) = msg.strip_prefix("Failed to parse JSONL at line ") {
        msg = rest.split_once(": ").map_or(rest, |(_, cause)| cause);
    }
    if let Some((cause, _)) = msg.split_once(" (salvaged ") {
        msg = cause;
    }
    if let Some(pos) = msg.rfind(" at line ") {
        let tail = &msg[pos + " at line ".len()..];
        if tail
            .split_once(" column ")
            .is_some_and(|(l, c)| l.parse::<u64>().is_ok() && c.parse::<u64>().is_ok())
        {
            msg = &msg[..pos];
        }
    }
    msg.trim().to_string()
}

/// Aggregated drift report.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    /// Sessions scanned.
    pub sessions_scanned: usize,
    /// Entries scanned (parsed + salvaged).
    pub entries_scanned: usize,
    /// Entry `type` values with no `LogEntry` variant (preserved as Unknown).
    pub unknown_entry_types: IndexMap<String, DriftSighting>,
    /// System subtypes carried by the `Other` catch-all.
    pub other_system_subtypes: IndexMap<String, DriftSighting>,
    /// Content-block types with no `ContentBlock` variant.
    pub unknown_content_blocks: IndexMap<String, DriftSighting>,
    /// All attachment kinds observed, flagged rendered vs marker-only.
    pub attachment_kinds: IndexMap<String, AttachmentSighting>,
    /// Models with usage but no pricing entry (cost estimates show N/A).
    pub unpriced_models: IndexMap<String, DriftSighting>,
    /// Thinking blocks observed.
    pub thinking_blocks: usize,
    /// Thinking blocks whose text is empty (recent Claude Code persists only
    /// the encrypted signature).
    pub thinking_blocks_empty: usize,
    /// Lines the lenient parser dropped.
    pub lines_unparsed: usize,
    /// Sessions containing at least one unparsed line.
    pub sessions_with_unparsed: usize,
    /// Entries recovered from torn lines.
    pub entries_salvaged: usize,
    /// Unparsed lines grouped by normalized error message.
    pub parse_errors: IndexMap<String, ParseIssueSighting>,
    /// Records kept by dropping malformed optional fields.
    pub entries_degraded: usize,
    /// Degraded records grouped by `<dropped fields>: <error>`.
    pub degraded_fields: IndexMap<String, ParseIssueSighting>,
}

impl DoctorReport {
    /// Share of thinking blocks with empty text, as a percentage.
    #[must_use]
    pub fn thinking_empty_pct(&self) -> f64 {
        if self.thinking_blocks == 0 {
            return 0.0;
        }
        (self.thinking_blocks_empty as f64 / self.thinking_blocks as f64) * 100.0
    }
}

#[derive(Debug, Default)]
struct SightingBuilder {
    count: usize,
    sessions: HashSet<String>,
    last_seen: Option<DateTime<Utc>>,
    example_session: Option<String>,
}

impl SightingBuilder {
    fn record(&mut self, session_id: &str, timestamp: Option<DateTime<Utc>>) {
        self.count += 1;
        if self.sessions.insert(session_id.to_string()) && self.example_session.is_none() {
            self.example_session = Some(session_id.to_string());
        }
        if let Some(ts) = timestamp {
            if self.last_seen.is_none_or(|prev| ts > prev) {
                self.last_seen = Some(ts);
            }
        }
    }

    fn finish(self) -> DriftSighting {
        DriftSighting {
            count: self.count,
            session_count: self.sessions.len(),
            last_seen: self.last_seen,
            example_session: self.example_session,
        }
    }
}

/// Accumulates drift observations across sessions; `finish()` yields the report.
#[derive(Debug, Default)]
pub struct Diagnoser {
    sessions_scanned: usize,
    entries_scanned: usize,
    unknown_entry_types: IndexMap<String, SightingBuilder>,
    other_system_subtypes: IndexMap<String, SightingBuilder>,
    unknown_content_blocks: IndexMap<String, SightingBuilder>,
    attachment_kinds: IndexMap<String, SightingBuilder>,
    attachment_rendered_counts: IndexMap<String, usize>,
    unpriced_models: IndexMap<String, SightingBuilder>,
    thinking_blocks: usize,
    thinking_blocks_empty: usize,
    lines_unparsed: usize,
    sessions_with_unparsed: usize,
    entries_salvaged: usize,
    parse_errors: IndexMap<String, IssueBuilder>,
    entries_degraded: usize,
    degraded_fields: IndexMap<String, IssueBuilder>,
}

impl Diagnoser {
    /// Create an empty diagnoser.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest one session's entries and parse stats.
    pub fn diagnose(&mut self, session_id: &str, entries: &[LogEntry], stats: &ParseStats) {
        self.sessions_scanned += 1;
        self.entries_scanned += entries.len();
        self.lines_unparsed += stats.lines_skipped;
        if stats.lines_skipped > 0 {
            self.sessions_with_unparsed += 1;
        }
        self.entries_salvaged += stats.entries_salvaged;
        for error in &stats.errors {
            self.parse_errors
                .entry(normalize_parse_error(&error.message))
                .or_default()
                .record(session_id, error.line);
        }
        self.entries_degraded += stats.degraded.len();
        for degraded in &stats.degraded {
            let key = format!(
                "{}: {}",
                degraded.degradation.dropped_fields.join(", "),
                normalize_parse_error(&degraded.degradation.error)
            );
            self.degraded_fields
                .entry(key)
                .or_default()
                .record(session_id, degraded.line);
        }

        for entry in entries {
            let ts = entry.timestamp();
            match entry {
                LogEntry::Unknown(value) => {
                    let kind = value
                        .get("type")
                        .and_then(|t| t.as_str())
                        .unwrap_or("(no type)");
                    self.unknown_entry_types
                        .entry(kind.to_string())
                        .or_default()
                        .record(session_id, ts);
                }
                LogEntry::System(sys) => {
                    if let Some(SystemSubtype::Other(name)) = &sys.subtype {
                        self.other_system_subtypes
                            .entry(name.clone())
                            .or_default()
                            .record(session_id, ts);
                    }
                }
                LogEntry::Attachment(att) => {
                    let kind = att
                        .attachment
                        .as_ref()
                        .and_then(|p| p.get("type"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("(no type)");
                    self.attachment_kinds
                        .entry(kind.to_string())
                        .or_default()
                        .record(session_id, ts);
                    let rendered = RENDERED_ATTACHMENT_KINDS.contains(&kind)
                        || crate::analysis::extraction::queued_human_prompt(entry).is_some();
                    if rendered {
                        *self
                            .attachment_rendered_counts
                            .entry(kind.to_string())
                            .or_default() += 1;
                    }
                }
                LogEntry::Assistant(assistant) => {
                    // "<synthetic>" is Claude Code's placeholder on synthetic
                    // (client-generated) messages, not a priceable model. Only
                    // usage-bearing messages affect cost estimates.
                    if ModelPricing::for_model(&assistant.message.model).is_none()
                        && !assistant.message.model.is_empty()
                        && assistant.message.model != "<synthetic>"
                        && assistant.message.usage.is_some()
                    {
                        self.unpriced_models
                            .entry(assistant.message.model.clone())
                            .or_default()
                            .record(session_id, ts);
                    }
                    for block in &assistant.message.content {
                        match block {
                            ContentBlock::Thinking(thinking) => {
                                self.thinking_blocks += 1;
                                if thinking.thinking.trim().is_empty() {
                                    self.thinking_blocks_empty += 1;
                                }
                            }
                            ContentBlock::Unknown { kind, .. } => {
                                self.record_unknown_block(kind, session_id, ts);
                            }
                            _ => {}
                        }
                    }
                }
                LogEntry::User(user) => {
                    if let UserContent::Blocks(blocks) = &user.message {
                        for block in &blocks.content {
                            if let ContentBlock::Unknown { kind, .. } = block {
                                self.record_unknown_block(kind, session_id, ts);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn record_unknown_block(&mut self, kind: &str, session_id: &str, ts: Option<DateTime<Utc>>) {
        let kind = if kind.is_empty() { "(no type)" } else { kind };
        self.unknown_content_blocks
            .entry(kind.to_string())
            .or_default()
            .record(session_id, ts);
    }

    /// Produce the final report, ordered most-frequent first.
    #[must_use]
    pub fn finish(self) -> DoctorReport {
        fn finalize(map: IndexMap<String, SightingBuilder>) -> IndexMap<String, DriftSighting> {
            let mut out: Vec<(String, DriftSighting)> =
                map.into_iter().map(|(k, v)| (k, v.finish())).collect();
            out.sort_by_key(|(_, s)| std::cmp::Reverse(s.count));
            out.into_iter().collect()
        }
        fn finalize_issues(
            map: IndexMap<String, IssueBuilder>,
        ) -> IndexMap<String, ParseIssueSighting> {
            let mut out: Vec<(String, ParseIssueSighting)> =
                map.into_iter().map(|(k, v)| (k, v.finish())).collect();
            out.sort_by_key(|(_, s)| std::cmp::Reverse(s.count));
            out.into_iter().collect()
        }

        let rendered_counts = self.attachment_rendered_counts;
        let attachment_kinds = finalize(self.attachment_kinds)
            .into_iter()
            .map(|(kind, sighting)| {
                let rendered_count = rendered_counts.get(&kind).copied().unwrap_or(0);
                (
                    kind,
                    AttachmentSighting {
                        rendered_count,
                        sighting,
                    },
                )
            })
            .collect();

        DoctorReport {
            sessions_scanned: self.sessions_scanned,
            entries_scanned: self.entries_scanned,
            unknown_entry_types: finalize(self.unknown_entry_types),
            other_system_subtypes: finalize(self.other_system_subtypes),
            unknown_content_blocks: finalize(self.unknown_content_blocks),
            attachment_kinds,
            unpriced_models: finalize(self.unpriced_models),
            thinking_blocks: self.thinking_blocks,
            thinking_blocks_empty: self.thinking_blocks_empty,
            lines_unparsed: self.lines_unparsed,
            sessions_with_unparsed: self.sessions_with_unparsed,
            entries_salvaged: self.entries_salvaged,
            parse_errors: finalize_issues(self.parse_errors),
            entries_degraded: self.entries_degraded,
            degraded_fields: finalize_issues(self.degraded_fields),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(lines: &str) -> (Vec<LogEntry>, ParseStats) {
        let mut parser = crate::parser::JsonlParser::new().with_lenient(true);
        let entries = parser.parse_str(lines).unwrap();
        (entries, parser.stats().clone())
    }

    #[test]
    fn test_diagnose_flags_unknown_types_and_thinking() {
        let lines = concat!(
            // Unknown entry type
            r#"{"type":"pr-link","sessionId":"s","prNumber":7,"prUrl":"https://x/7","timestamp":"2026-07-01T00:00:00Z"}"#,
            "\n",
            // System subtype via Other
            r#"{"type":"system","subtype":"away_summary","uuid":"y1","timestamp":"2026-07-02T00:00:00Z","sessionId":"s","content":"away"}"#,
            "\n",
            // Assistant with empty thinking + unpriced model
            r#"{"type":"assistant","uuid":"a1","parentUuid":null,"timestamp":"2026-07-03T00:00:00Z","sessionId":"s","isSidechain":false,"userType":"external","cwd":"/","version":"2.1.198","gitBranch":"main","message":{"id":"m1","type":"message","role":"assistant","model":"claude-future-9","content":[{"type":"thinking","thinking":"","signature":"sig"},{"type":"mystery_block","payload":1}],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
            "\n",
            // Unknown model WITHOUT usage — must not count as unpriced
            r#"{"type":"assistant","uuid":"a2","parentUuid":"a1","timestamp":"2026-07-03T00:00:01Z","sessionId":"s","isSidechain":false,"userType":"external","cwd":"/","version":"2.1.198","gitBranch":"main","message":{"id":"m2","type":"message","role":"assistant","model":"claude-usage-less","content":[{"type":"text","text":"x"}]}}"#,
            "\n",
            // Attachment kind (marker-only)
            r#"{"uuid":"t1","type":"attachment","timestamp":"2026-07-04T00:00:00Z","sessionId":"s","attachment":{"type":"total_tokens_reminder","text":"x"}}"#,
            "\n",
            // queued_command: one human prompt (renders), one notification (marker-only)
            r#"{"uuid":"q1","type":"attachment","timestamp":"2026-07-04T01:00:00Z","sessionId":"s","attachment":{"type":"queued_command","commandMode":"prompt","prompt":"note this"}}"#,
            "\n",
            r#"{"uuid":"q2","type":"attachment","timestamp":"2026-07-04T02:00:00Z","sessionId":"s","attachment":{"type":"queued_command","commandMode":"task-notification","prompt":"<task-notification>x</task-notification>"}}"#,
        );
        let (entries, stats) = parse(lines);
        let mut diagnoser = Diagnoser::new();
        diagnoser.diagnose("s", &entries, &stats);
        let report = diagnoser.finish();

        assert_eq!(report.sessions_scanned, 1);
        assert_eq!(report.unknown_entry_types["pr-link"].count, 1);
        assert!(report.unknown_entry_types["pr-link"].last_seen.is_some());
        assert_eq!(report.other_system_subtypes["away_summary"].count, 1);
        assert_eq!(report.unknown_content_blocks["mystery_block"].count, 1);
        assert_eq!(report.unpriced_models["claude-future-9"].count, 1);
        // No usage → does not affect cost estimates → not reported.
        assert!(!report.unpriced_models.contains_key("claude-usage-less"));
        assert_eq!(
            report.attachment_kinds["total_tokens_reminder"].rendered_count,
            0
        );
        // queued_command renders conditionally: the human prompt counts, the
        // task notification does not.
        let queued = &report.attachment_kinds["queued_command"];
        assert_eq!(queued.sighting.count, 2);
        assert_eq!(queued.rendered_count, 1);
        assert_eq!(report.thinking_blocks, 1);
        assert_eq!(report.thinking_blocks_empty, 1);
        assert!((report.thinking_empty_pct() - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_diagnose_counts_parse_degradation() {
        let complete = r#"{"uuid":"kept","parentUuid":null,"type":"user","timestamp":"2025-12-23T00:00:00Z","sessionId":"s","version":"2.0.74","isSidechain":false,"message":{"role":"user","content":"ok"}}"#;
        let torn = format!(r#"{{"type":"attachment","uuid":"trunc{complete}"#);
        let (entries, stats) = parse(&torn);
        let mut diagnoser = Diagnoser::new();
        diagnoser.diagnose("s", &entries, &stats);
        let report = diagnoser.finish();

        assert_eq!(report.lines_unparsed, 1);
        assert_eq!(report.sessions_with_unparsed, 1);
        assert_eq!(report.entries_salvaged, 1);
        assert_eq!(report.entries_scanned, 1);
    }

    #[test]
    fn test_normalize_parse_error_groups_by_cause() {
        assert_eq!(
            normalize_parse_error("Failed to parse JSONL at line 992: missing field `exitCode`"),
            "missing field `exitCode`"
        );
        assert_eq!(
            normalize_parse_error(
                "Failed to parse JSONL at line 7: expected `,` or `}` at line 1 column 5013 (salvaged 1 complete trailing entry from torn line)"
            ),
            "expected `,` or `}`"
        );
        // Text that merely contains " at line " is left alone.
        assert_eq!(
            normalize_parse_error("unknown variant `stopped at line two`"),
            "unknown variant `stopped at line two`"
        );
    }

    #[test]
    fn test_diagnose_groups_parse_errors_and_degraded_fields() {
        let good = r#"{"uuid":"1","parentUuid":null,"type":"user","timestamp":"2026-11-02T00:00:00Z","sessionId":"s","version":"2.1.316","isSidechain":false,"message":{"role":"user","content":"ok"}}"#;
        let degraded = r#"{"uuid":"2","parentUuid":"1","type":"system","subtype":"stop_hook_summary","timestamp":"2026-11-02T00:00:01Z","sessionId":"s","isSidechain":false,"hookInfos":[{"command":"x","durationMs":"59ms"}]}"#;
        let (entries, stats) = parse(&format!("{good}\n{degraded}\n{{\"type\":\n{{\"type\":\n"));
        let mut diagnoser = Diagnoser::new();
        diagnoser.diagnose("s1", &entries, &stats);
        let report = diagnoser.finish();

        assert_eq!(report.lines_unparsed, 2);
        assert_eq!(report.parse_errors.len(), 1, "{:?}", report.parse_errors);
        let (cause, sighting) = report.parse_errors.first().unwrap();
        assert!(cause.starts_with("EOF while parsing"), "{cause}");
        assert_eq!(sighting.count, 2);
        assert_eq!(sighting.example.as_deref(), Some("s1:3"));

        assert_eq!(report.entries_degraded, 1);
        let (cause, sighting) = report.degraded_fields.first().unwrap();
        assert!(cause.starts_with("hookInfos: invalid type"), "{cause}");
        assert_eq!(sighting.example.as_deref(), Some("s1:2"));
    }
}
