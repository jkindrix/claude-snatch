//! Session lesson extraction: error→fix pairs and user corrections.
//!
//! Targets the most expensive compaction failure modes:
//! - **F2 (Negative result amnesia)**: Recovers what failed and how it was fixed
//! - **F4 (Operational gotcha amnesia)**: Recovers user corrections of agent behavior
//!
//! # Usage
//!
//! ```rust,no_run
//! use claude_snatch::analysis::lessons::{extract_lessons, LessonOptions};
//! use claude_snatch::reconstruction::Conversation;
//!
//! # fn example(conversation: &Conversation) {
//! let entries = conversation.chronological_entries();
//! let options = LessonOptions::default();
//! let result = extract_lessons(&entries, &options);
//! println!("Found {} errors, {} corrections",
//!     result.error_fix_pairs.len(),
//!     result.user_corrections.len());
//! # }
//! ```

use std::collections::HashMap;

use crate::model::content::{ToolResult, ToolResultContent};
use crate::model::message::LogEntry;
use crate::provider::{ActivityKind, PromptAuthorship, ToolKind, ToolSemantics};
use crate::reconstruction::Conversation;

use super::extraction::{
    extract_assistant_summary, extract_tool_input_summary, is_human_prompt, primary_content_text,
    truncate_text,
};

/// Extract plain text from a ToolResultContent value.
fn tool_result_text(content: &ToolResultContent) -> String {
    match content {
        ToolResultContent::String(s) => s.clone(),
        ToolResultContent::Array(arr) => {
            // Extract text from array elements (typically {"type":"text","text":"..."})
            arr.iter()
                .filter_map(|v| v.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        }
    }
}

// ── Result types ────────────────────────────────────────────────────────────

/// An error→fix pair: a tool call that failed, and what happened next.
#[derive(Debug, Clone)]
pub struct ErrorFixPair {
    /// When the error occurred (RFC 3339).
    pub timestamp: Option<String>,
    /// The tool that errored.
    pub tool_name: String,
    /// Key input fields for the failing call.
    pub input_summary: HashMap<String, String>,
    /// Preview of the error message.
    pub error_preview: String,
    /// Confidence tier of the failure classification.
    pub failure_kind: FailureKind,
    /// Evidence used to classify the failure.
    pub failure_basis: FailureBasis,
    /// What the assistant did next (text summary of next response).
    pub resolution_summary: Option<String>,
    /// Tools used in the resolution attempt.
    pub resolution_tools: Vec<String>,
}

/// A user correction: where the user corrected the agent's behavior.
#[derive(Debug, Clone)]
pub struct UserCorrectionEntry {
    /// When the correction was made (RFC 3339).
    pub timestamp: Option<String>,
    /// The user's correction text.
    pub user_text: String,
    /// What the assistant was doing before (summary of previous response).
    pub prior_assistant_summary: Option<String>,
    /// High-precision dialogue signal that admitted this correction.
    pub correction_basis: CorrectionBasis,
}

/// Summary statistics for extracted lessons.
#[derive(Debug, Clone)]
pub struct LessonsSummary {
    /// Total error→fix pairs found.
    pub total_errors: usize,
    /// Failures backed by a native flag, process status, or structured error.
    pub confirmed_tool_failures: usize,
    /// Error-like output inferred from text without an authoritative status.
    pub inferred_failure_signals: usize,
    /// Total user corrections found.
    pub total_corrections: usize,
    /// Tools ranked by error frequency (most error-prone first).
    pub most_error_prone_tools: Vec<(String, usize)>,
}

/// Confidence tier for one tool-result failure classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// Backed by an authoritative native or structured status.
    Confirmed,
    /// Inferred from unstructured result text.
    Inferred,
}

impl FailureKind {
    /// Stable human/wire label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Inferred => "inferred",
        }
    }
}

/// Evidence used to classify a tool result as a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureBasis {
    /// Native `is_error=true` marker.
    NativeErrorFlag,
    /// Explicit nonzero process exit status.
    ProcessExit,
    /// Provider-native lifecycle status or success flag.
    NativeLifecycleStatus,
    /// Structured response with a non-null/non-false error field.
    StructuredError,
    /// Unstructured error-signature heuristic.
    TextSignature,
}

impl FailureBasis {
    /// Stable human/wire label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NativeErrorFlag => "native error flag",
            Self::ProcessExit => "process exit",
            Self::NativeLifecycleStatus => "native lifecycle status",
            Self::StructuredError => "structured error",
            Self::TextSignature => "text signature",
        }
    }
}

/// Classification of one tool result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailureClassification {
    /// Confidence tier.
    pub kind: FailureKind,
    /// Evidence basis.
    pub basis: FailureBasis,
}

/// Counts under the shared failure taxonomy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FailureCounts {
    /// Authoritatively confirmed tool failures.
    pub confirmed: usize,
    /// Heuristically inferred failure signals.
    pub inferred: usize,
}

/// Evidence that a human message is correcting the preceding assistant.
///
/// Corrections are pragmatic dialogue acts, not occurrences of words such as
/// "already" or "again". These categories intentionally favor precision over
/// recall so ordinary collaborative prose is not mislabeled as a correction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionBasis {
    /// Direct rejection such as "No, ..." or "That's not what I asked."
    ExplicitRejection,
    /// A direct instruction to stop, resume, or change the previous action.
    BehavioralRedirect,
    /// A clarification of the user's previously expressed intent.
    IntentClarification,
    /// Criticism of the assistant's prior work, reasoning, or behavior.
    PerformanceCritique,
}

impl CorrectionBasis {
    /// Stable human-facing label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitRejection => "explicit rejection",
            Self::BehavioralRedirect => "behavioral redirect",
            Self::IntentClarification => "intent clarification",
            Self::PerformanceCritique => "performance critique",
        }
    }
}

impl FailureCounts {
    /// All classified failure incidents.
    #[must_use]
    pub const fn total(self) -> usize {
        self.confirmed + self.inferred
    }
}

/// Complete lesson extraction result.
#[derive(Debug, Clone)]
pub struct LessonResult {
    /// Error→fix pairs found in the session.
    pub error_fix_pairs: Vec<ErrorFixPair>,
    /// User corrections found in the session.
    pub user_corrections: Vec<UserCorrectionEntry>,
    /// Summary statistics.
    pub summary: LessonsSummary,
}

// ── Options ─────────────────────────────────────────────────────────────────

/// Which categories of lessons to extract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LessonCategory {
    /// Only error→fix pairs.
    Errors,
    /// Only user corrections.
    Corrections,
    /// Both.
    All,
}

impl LessonCategory {
    /// Parse from string (e.g., "errors", "corrections", "all").
    pub fn from_str_loose(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "errors" => Self::Errors,
            "corrections" => Self::Corrections,
            _ => Self::All,
        }
    }
}

/// Options controlling lesson extraction behavior.
#[derive(Debug, Clone)]
pub struct LessonOptions {
    /// Which categories to extract.
    pub category: LessonCategory,
    /// Maximum lessons per category.
    pub limit: usize,
    /// Max chars for error preview text.
    pub error_preview_len: usize,
    /// Max chars for resolution summary text.
    pub resolution_summary_len: usize,
    /// Max chars for user correction text.
    pub correction_text_len: usize,
}

impl Default for LessonOptions {
    fn default() -> Self {
        Self {
            category: LessonCategory::All,
            limit: 30,
            error_preview_len: 300,
            resolution_summary_len: 200,
            correction_text_len: 300,
        }
    }
}

// ── Core extraction ─────────────────────────────────────────────────────────

/// Check if text starts with a line-number prefix (e.g., "     1→", "787→").
/// This is the format used by Read tool output.
fn starts_with_line_number(s: &str) -> bool {
    let trimmed = s.trim_start();
    let digit_end = trimmed.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
    digit_end > 0 && {
        // Read output prefixes line numbers with either an arrow ("1→", older
        // format) or a tab ("1\t", current cat -n format). Accept both, or every
        // line-numbered file read whose content trips the soft-error regex is
        // misclassified as an error.
        let rest = &trimmed[digit_end..];
        rest.starts_with('→') || rest.starts_with('\t')
    }
}

/// Check if text starts with grep-style line output (e.g., "21:", "21-").
fn starts_with_grep_line(s: &str) -> bool {
    let trimmed = s.trim_start();
    let digit_end = trimmed.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
    digit_end > 0 && {
        let rest = &trimmed[digit_end..];
        rest.starts_with(':') || rest.starts_with('-')
    }
}

/// Heuristic: detect tool result content that looks like a successful result
/// despite having `is_error=true`. This filters false positives from Claude Code's
/// JSONL logging where certain tool results are spuriously flagged.
fn is_likely_false_positive(tool_name: &str, content: &str) -> bool {
    match tool_name {
        // Read results starting with line-numbered content (N→) are successful file reads
        // Handles both "1→" (start of file) and "787→" (offset reads)
        "Read" => starts_with_line_number(content),
        // Grep results starting with line-numbered output are successful searches
        "Grep" => starts_with_grep_line(content),
        // MCP tool results returning valid JSON objects/arrays are successful calls
        name if name.starts_with("mcp__") => {
            let trimmed = content.trim_start();
            trimmed.starts_with('{') || trimmed.starts_with('[')
        }
        // Agent results with substantial text that don't start with error markers
        "Agent" => content.len() > 200 && !content.starts_with("Error"),
        _ => false,
    }
}

/// Read-only git commands whose output (commit messages, diffs) routinely
/// contains error-like words without being a tool failure — e.g. `git log`
/// showing a "fix stack overflow" commit message would otherwise match the
/// soft-error regex.
fn is_readonly_git_command(command: &str) -> bool {
    let c = command.trim_start();
    c.starts_with("git log")
        || c.starts_with("git show")
        || c.starts_with("git diff")
        || c.starts_with("git blame")
        || c.starts_with("git status")
}

/// Extract a shell command from the provider-normalized tool input.
fn shell_command(input: &serde_json::Value) -> Option<String> {
    for key in ["cmd", "command"] {
        match input.get(key) {
            Some(serde_json::Value::String(command)) => return Some(command.clone()),
            Some(serde_json::Value::Array(parts)) => {
                return Some(
                    parts
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
            _ => {}
        }
    }
    None
}

/// Commands whose successful output is content rather than an operation
/// result. Error-looking text inside that content is not evidence of failure.
fn is_observational_shell_command(command: &str) -> bool {
    let command = command.trim_start();
    [
        "cat ",
        "sed ",
        "rg ",
        "grep ",
        "head ",
        "tail ",
        "find ",
        "ls ",
        "wc ",
        "git log",
        "git show",
        "git diff",
        "git blame",
        "git status",
    ]
    .iter()
    .any(|prefix| command.starts_with(prefix))
}

fn process_exit_code(content: &str) -> Option<i32> {
    let line_status = content.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix("Process exited with code ")
            .or_else(|| line.strip_prefix("Exit code: "))
            .and_then(|code| code.trim().parse().ok())
    });
    line_status.or_else(|| {
        let value = serde_json::from_str::<serde_json::Value>(content).ok()?;
        value
            .get("exit_code")
            .or_else(|| value.pointer("/metadata/exit_code"))
            .and_then(serde_json::Value::as_i64)
            .and_then(|code| i32::try_from(code).ok())
    })
}

fn structured_error(content: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|value| value.get("error").cloned())
        .is_some_and(|error| !error.is_null() && error != false)
}

/// Provider-semantic error classification grounded in the Codex corpus shape
/// census. Explicit process status is authoritative; cumulative/plain output
/// content is not treated as an error merely because it contains source code,
/// compiler diagnostics, or words such as "panic".
fn semantic_tool_result_failure(
    semantics: &ToolSemantics,
    input: &serde_json::Value,
    content: &str,
    soft_error_re: Option<&regex::Regex>,
) -> Option<FailureClassification> {
    match lifecycle_verdict(semantics, input) {
        LifecycleVerdict::Failed(basis) => {
            return Some(FailureClassification {
                kind: FailureKind::Confirmed,
                basis,
            });
        }
        LifecycleVerdict::Succeeded => return None,
        LifecycleVerdict::Inconclusive => {}
    }
    match &semantics.kind {
        ToolKind::Shell => {
            let command = shell_command(input);
            if let Some(exit) = process_exit_code(content) {
                if exit == 0 {
                    return None;
                }
                // `rg`/`grep` use 1 for an ordinary no-match result.
                if exit == 1
                    && command.as_deref().is_some_and(|command| {
                        let command = command.trim_start();
                        command.starts_with("rg ") || command.starts_with("grep ")
                    })
                {
                    return None;
                }
                return Some(FailureClassification {
                    kind: FailureKind::Confirmed,
                    basis: FailureBasis::ProcessExit,
                });
            }
            if content.contains("Process running with session ID") {
                return None;
            }
            if command
                .as_deref()
                .is_some_and(is_observational_shell_command)
            {
                return None;
            }
            if structured_error(content) {
                Some(FailureClassification {
                    kind: FailureKind::Confirmed,
                    basis: FailureBasis::StructuredError,
                })
            } else if content.trim_start().starts_with("Error:")
                || soft_error_re.is_some_and(|re| re.is_match(content))
            {
                Some(FailureClassification {
                    kind: FailureKind::Inferred,
                    basis: FailureBasis::TextSignature,
                })
            } else {
                None
            }
        }
        ToolKind::FileWrite => {
            if structured_error(content) {
                Some(FailureClassification {
                    kind: FailureKind::Confirmed,
                    basis: FailureBasis::StructuredError,
                })
            } else if content.contains("apply_patch verification failed")
                || content.contains("Failed to find expected")
                || content.contains("Invalid Context")
                || content.contains("Patch failed")
            {
                Some(FailureClassification {
                    kind: FailureKind::Inferred,
                    basis: FailureBasis::TextSignature,
                })
            } else {
                None
            }
        }
        ToolKind::Mcp | ToolKind::Web | ToolKind::Orchestration | ToolKind::Other(_) => {
            structured_error(content).then_some(FailureClassification {
                kind: FailureKind::Confirmed,
                basis: FailureBasis::StructuredError,
            })
        }
        // Read/search results routinely contain source text that matches the
        // soft regex. Only the provider's hard-error bit (handled by the
        // caller) is authoritative for those content-bearing tools.
        ToolKind::FileRead | ToolKind::Search | ToolKind::Subagent => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleVerdict {
    Succeeded,
    Failed(FailureBasis),
    Inconclusive,
}

/// Reduce typed lifecycle observations without consulting output text. Any
/// explicit failure wins; otherwise at least one positive completion signal
/// establishes success. Web observations intentionally remain inconclusive
/// because their native end record has no status/success field.
fn lifecycle_verdict(semantics: &ToolSemantics, input: &serde_json::Value) -> LifecycleVerdict {
    let mut completed = false;
    for observation in &semantics.lifecycle {
        if let Some(exit_code) = observation.exit_code {
            completed = true;
            if exit_code != 0 {
                // rg/grep exit 1 is a normal no-match result, including when
                // the provider labels the underlying process "failed".
                let no_match = exit_code == 1
                    && shell_command(input).as_deref().is_some_and(|command| {
                        let command = command.trim_start();
                        command.starts_with("rg ") || command.starts_with("grep ")
                    });
                if !no_match {
                    return LifecycleVerdict::Failed(FailureBasis::ProcessExit);
                }
            }
        }
        if observation.success == Some(false)
            || matches!(
                observation.status,
                Some(
                    crate::provider::ToolExecutionStatus::Failed
                        | crate::provider::ToolExecutionStatus::Declined
                )
            )
        {
            // A no-match process status is the one deliberate exception to
            // the provider's generic failed label.
            let no_match = observation.exit_code == Some(1)
                && shell_command(input).as_deref().is_some_and(|command| {
                    let command = command.trim_start();
                    command.starts_with("rg ") || command.starts_with("grep ")
                });
            if !no_match {
                return LifecycleVerdict::Failed(FailureBasis::NativeLifecycleStatus);
            }
        }
        completed |= observation.success == Some(true)
            || matches!(
                observation.status,
                Some(crate::provider::ToolExecutionStatus::Completed)
            );
    }
    if completed {
        LifecycleVerdict::Succeeded
    } else {
        LifecycleVerdict::Inconclusive
    }
}

#[cfg(test)]
fn semantic_tool_result_is_error(
    semantics: &ToolSemantics,
    input: &serde_json::Value,
    content: &str,
    soft_error_re: Option<&regex::Regex>,
) -> bool {
    semantic_tool_result_failure(semantics, input, content, soft_error_re).is_some()
}

#[derive(Default)]
struct SemanticLessonContext {
    tools: HashMap<String, ToolSemantics>,
}

/// Soft error pattern: detect errors in tool result content even when
/// `is_error` is not set (e.g., SIGSEGV, panics, assertion failures).
fn build_soft_error_regex() -> Option<regex::Regex> {
    regex::RegexBuilder::new(
        r"(?:Segmentation fault|SIGSEGV|SIGABRT|panic|stack overflow|assertion failed|fatal error|thread .* panicked|Exit code (?:[1-9]\d*|1\d\d)|error\[E\d+\]|cannot find|unresolved|undefined reference)"
    )
    .case_insensitive(true)
    .build()
    .ok()
}

/// Compiled dialogue-act patterns for high-precision correction detection.
struct CorrectionPatterns {
    explicit_rejection: regex::Regex,
    behavioral_redirect: regex::Regex,
    intent_clarification: regex::Regex,
    performance_critique: regex::Regex,
}

fn build_correction_patterns() -> Option<CorrectionPatterns> {
    let compile = |pattern| {
        regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .multi_line(true)
            .build()
            .ok()
    };

    Some(CorrectionPatterns {
        // Sentence-level rejection avoids matching phrases such as "No action
        // needed" while retaining "No, not yet" and "No. That is wrong."
        explicit_rejection: compile(
            r"(?:^|[.!?]\s+)(?:(?:no|nope|nah)\s*[,!.:](?:\s|$)|(?:that(?:'s| is)|this is|you(?:'re| are))\s+(?:wrong|incorrect|mistaken|not\s+(?:what|the|right)\b))|\bnot what i (?:asked|requested|said|meant|wanted)\b",
        )?,
        behavioral_redirect: compile(
            r"(?:^|[.!?;]\s+)(?:please\s+)?stop\s+\w|(?:^|[.!?;]\s+)(?:please\s+)?never\s+(?:do|repeat|claim|try|use)\b|\bthis time\s+(?:don'?t|do not|stop|never)\s+\w|(?:^|[.!?;]\s+)(?:please\s+)?(?:continue|resume)\s+the\s+(?:previous|original|last)(?:\s+[\w.-]+){0,8}\s+(?:request|instruction|task)\b|(?:^|[.!?;]\s+)(?:please\s+)?(?:do|use|try|take|focus|proceed|leave|keep)\b[^.!?\n]{0,120}\binstead\b",
        )?,
        intent_clarification: compile(
            r"\bwhat i\s+(?:actually|really)\s+(?:asked|requested|said|meant|wanted)\b|\bi\s+(?:didn'?t|did not)\s+(?:ask|request|say|mean|want)\b|\b(?:previous|original|last)(?:\s+[\w.-]+){0,8}\s+(?:request|instruction|task)\b[^.!?\n]{0,40}\bexactly\b",
        )?,
        performance_critique: compile(
            r"\bwhy\s+(?:did|are|do|don'?t|didn'?t|not)\s+you\b|\bhow many times\b|\byou\s+(?:keep|kept|failed|forgot|ignored|missed|misread|misunderstood|misinterpreted|didn'?t|did not|haven't|have not|weren't|were not|aren't|are not|can'?t|cannot)\b|\b(?:issues?|problems?)\s+with\s+your\s+(?:work|answer|response|analysis|implementation)\b|\byour\s+(?:work|answer|response|analysis|implementation)\b[^.!?\n]{0,80}\b(?:wrong|incorrect|incomplete|missing|broken|flawed)\b|\bi\s+(?:don'?t|do not)\s+understand\s+(?:your|why you|what you)\b|\b(?:losing faith|wasting (?:my )?time|sick of|frustrat(?:ed|ing|ion))\b",
        )?,
    })
}

/// Classify a correction from one coherent set of signals. The returned score
/// is also the ranking score, preventing the former gate/ranker disagreement.
fn classify_correction(
    text: &str,
    patterns: &CorrectionPatterns,
) -> Option<(u32, CorrectionBasis)> {
    let signals = [
        (
            patterns.explicit_rejection.is_match(text),
            400,
            CorrectionBasis::ExplicitRejection,
        ),
        (
            patterns.performance_critique.is_match(text),
            350,
            CorrectionBasis::PerformanceCritique,
        ),
        (
            patterns.intent_clarification.is_match(text),
            300,
            CorrectionBasis::IntentClarification,
        ),
        (
            patterns.behavioral_redirect.is_match(text),
            250,
            CorrectionBasis::BehavioralRedirect,
        ),
    ];
    let signal_count = signals.iter().filter(|(matched, _, _)| *matched).count() as u32;
    signals
        .into_iter()
        .find(|(matched, _, _)| *matched)
        .map(|(_, base, basis)| (base + signal_count, basis))
}

fn classify_tool_result_with(
    tool_name: &str,
    input: &serde_json::Value,
    result: &ToolResult,
    semantics: Option<&ToolSemantics>,
    semantic_annotations: bool,
    soft_error_re: Option<&regex::Regex>,
) -> Option<FailureClassification> {
    let content = result.content.as_ref().map(tool_result_text);

    if let Some(semantics) = semantics {
        if let LifecycleVerdict::Failed(basis) = lifecycle_verdict(semantics, input) {
            return Some(FailureClassification {
                kind: FailureKind::Confirmed,
                basis,
            });
        }
    }

    if result.is_error == Some(true) {
        if semantics.is_none()
            && content
                .as_deref()
                .is_some_and(|text| is_likely_false_positive(tool_name, text))
        {
            return None;
        }
        return Some(FailureClassification {
            kind: FailureKind::Confirmed,
            basis: FailureBasis::NativeErrorFlag,
        });
    }

    if let Some(semantics) = semantics {
        return content.as_deref().and_then(|content| {
            semantic_tool_result_failure(semantics, input, content, soft_error_re)
        });
    }

    let content = content.as_deref()?;
    if semantic_annotations {
        if structured_error(content) {
            return Some(FailureClassification {
                kind: FailureKind::Confirmed,
                basis: FailureBasis::StructuredError,
            });
        }
        return content
            .trim_start()
            .starts_with("Error:")
            .then_some(FailureClassification {
                kind: FailureKind::Inferred,
                basis: FailureBasis::TextSignature,
            });
    }
    if tool_name == "Bash"
        && input
            .get("command")
            .and_then(serde_json::Value::as_str)
            .is_some_and(is_readonly_git_command)
    {
        return None;
    }
    if structured_error(content) {
        return Some(FailureClassification {
            kind: FailureKind::Confirmed,
            basis: FailureBasis::StructuredError,
        });
    }
    soft_error_re
        .is_some_and(|re| re.is_match(content))
        .then_some(FailureClassification {
            kind: FailureKind::Inferred,
            basis: FailureBasis::TextSignature,
        })
}

/// Classify one linked tool result under the shared failure taxonomy.
#[must_use]
pub fn classify_tool_result(
    tool_name: &str,
    input: &serde_json::Value,
    result: &ToolResult,
    semantics: Option<&ToolSemantics>,
    semantic_annotations: bool,
) -> Option<FailureClassification> {
    let soft_error_re = build_soft_error_regex();
    classify_tool_result_with(
        tool_name,
        input,
        result,
        semantics,
        semantic_annotations,
        soft_error_re.as_ref(),
    )
}

/// Provider tool semantics keyed by native call id.
#[must_use]
pub fn conversation_tool_semantics(conversation: &Conversation) -> HashMap<String, ToolSemantics> {
    let mut tools = HashMap::new();
    let Some(bundle) = conversation.provider_bundle() else {
        return tools;
    };
    for entry_semantics in bundle.semantics.values() {
        for (call_id, tool) in &entry_semantics.tools {
            tools.entry(call_id.clone()).or_insert_with(|| tool.clone());
        }
    }
    tools
}

/// Count classified tool-result failures across an explicit entry scope.
#[must_use]
pub fn count_tool_failures<S: std::hash::BuildHasher>(
    entries: &[&LogEntry],
    semantics: &HashMap<String, ToolSemantics, S>,
    semantic_annotations: bool,
) -> FailureCounts {
    let mut calls: HashMap<String, (String, serde_json::Value)> = HashMap::new();
    for entry in entries {
        if let LogEntry::Assistant(assistant) = entry {
            for tool in assistant.message.tool_uses() {
                calls.insert(tool.id.clone(), (tool.name.clone(), tool.input.clone()));
            }
        }
    }
    let soft_error_re = build_soft_error_regex();
    let mut counts = FailureCounts::default();
    for entry in entries {
        let LogEntry::User(user) = entry else {
            continue;
        };
        for result in user.message.tool_results() {
            let (tool_name, input) = calls
                .get(&result.tool_use_id)
                .cloned()
                .unwrap_or_else(|| ("unknown".to_string(), serde_json::Value::Null));
            if let Some(classification) = classify_tool_result_with(
                &tool_name,
                &input,
                result,
                semantics.get(&result.tool_use_id),
                semantic_annotations,
                soft_error_re.as_ref(),
            ) {
                match classification.kind {
                    FailureKind::Confirmed => counts.confirmed += 1,
                    FailureKind::Inferred => counts.inferred += 1,
                }
            }
        }
    }
    counts
}

/// Check if a user message is a compaction/continuation summary (false positive filter).
fn is_summary_text(text: &str) -> bool {
    text.len() > 3000
        || text.contains("All User Messages:")
        || text.contains("conversation that ran out of context")
        || text.contains("Key Technical Concepts:")
}

/// Extract error→fix pairs from a chronological entry list.
///
/// Detects both hard errors (`is_error=true`) and soft errors (error patterns
/// in tool result content like SIGSEGV, panics, compiler errors).
/// For each error, captures the next assistant response as the resolution.
pub fn extract_error_fix_pairs(entries: &[&LogEntry], opts: &LessonOptions) -> Vec<ErrorFixPair> {
    extract_error_fix_pairs_with(entries, opts, None)
}

fn extract_error_fix_pairs_with(
    entries: &[&LogEntry],
    opts: &LessonOptions,
    semantic: Option<&SemanticLessonContext>,
) -> Vec<ErrorFixPair> {
    let soft_error_re = build_soft_error_regex();

    // Build map: tool_use_id → (tool_name, input, timestamp)
    let mut tool_use_map: HashMap<String, (String, serde_json::Value, Option<String>)> =
        HashMap::new();
    for entry in entries {
        if let LogEntry::Assistant(a) = entry {
            let ts = entry.timestamp().map(|t| t.to_rfc3339());
            for tool in a.message.tool_uses() {
                tool_use_map.insert(
                    tool.id.clone(),
                    (tool.name.clone(), tool.input.clone(), ts.clone()),
                );
            }
        }
    }

    let mut pairs = Vec::new();
    let mut i = 0;

    while i < entries.len() {
        if let LogEntry::User(user) = entries[i] {
            for result in user.message.tool_results() {
                // Look up the tool before classifying content: provider
                // semantics distinguish shell status wrappers, file-write
                // failures, and content-bearing read/search/web outputs.
                let (tool_name, input, timestamp) = tool_use_map
                    .get(&result.tool_use_id)
                    .cloned()
                    .unwrap_or_else(|| ("unknown".into(), serde_json::Value::Null, None));
                let content_text = result.content.as_ref().map(tool_result_text);
                let classification = classify_tool_result_with(
                    &tool_name,
                    &input,
                    result,
                    semantic.and_then(|context| context.tools.get(&result.tool_use_id)),
                    semantic.is_some(),
                    soft_error_re.as_ref(),
                );
                let Some(classification) = classification else {
                    continue;
                };

                let error_preview = content_text
                    .as_deref()
                    .map(|t| truncate_text(t, opts.error_preview_len))
                    .unwrap_or_else(|| "(error with no content)".into());

                let input_summary = extract_tool_input_summary(&tool_name, &input);

                // Look ahead for the next assistant message as resolution
                let mut resolution_summary = None;
                let mut resolution_tools = Vec::new();
                #[allow(clippy::needless_range_loop)]
                for j in (i + 1)..entries.len() {
                    if let LogEntry::Assistant(a) = entries[j] {
                        let text = a.message.combined_text();
                        let trimmed = text.trim();
                        resolution_summary = if trimmed.is_empty() {
                            None
                        } else {
                            Some(truncate_text(trimmed, opts.resolution_summary_len))
                        };
                        resolution_tools = a
                            .message
                            .tool_uses()
                            .iter()
                            .map(|t| t.name.clone())
                            .collect();
                        break;
                    }
                }

                pairs.push(ErrorFixPair {
                    timestamp,
                    tool_name,
                    input_summary,
                    error_preview,
                    failure_kind: classification.kind,
                    failure_basis: classification.basis,
                    resolution_summary,
                    resolution_tools,
                });
            }
        }
        i += 1;
    }

    pairs
}

/// Extract user corrections from a chronological entry list.
///
/// Detects user messages containing frustration, behavioral correction,
/// or explicit instructions to change approach. Filters out compaction
/// summaries and session continuation text to avoid false positives.
pub fn extract_user_corrections(
    entries: &[&LogEntry],
    opts: &LessonOptions,
) -> Vec<UserCorrectionEntry> {
    extract_user_corrections_with(entries, opts, is_human_prompt)
}

fn extract_user_corrections_with<Human>(
    entries: &[&LogEntry],
    opts: &LessonOptions,
    is_human: Human,
) -> Vec<UserCorrectionEntry>
where
    Human: Fn(&LogEntry) -> bool,
{
    let correction_patterns = match build_correction_patterns() {
        Some(patterns) => patterns,
        None => return Vec::new(),
    };

    // Collect candidates with a correction-strength score so stronger
    // corrections rank first and survive truncation (issue #26).
    let mut scored: Vec<(u32, UserCorrectionEntry)> = Vec::new();
    let mut prev_assistant_summary: Option<String> = None;

    for entry in entries {
        match entry {
            LogEntry::Assistant(_) => {
                prev_assistant_summary =
                    extract_assistant_summary(entry, opts.resolution_summary_len);
            }
            LogEntry::User(_) => {
                // Skip harness-injected/templated content (command echoes,
                // local-command-stdout hook output, system-reminder blocks,
                // tool-result turns, compaction summaries, isMeta entries) so
                // corrections reflect actual human pushback rather than
                // boilerplate. See #26.
                if !is_human(entry) {
                    continue;
                }
                // A correction is relational: without an assistant response to
                // repair, this is an initial instruction or task constraint.
                let Some(prior_assistant_summary) = prev_assistant_summary.clone() else {
                    continue;
                };
                if let Some(text) = primary_content_text(entry) {
                    if text.len() <= 10 || is_summary_text(&text) {
                        continue;
                    }
                    if let Some((strength, correction_basis)) =
                        classify_correction(&text, &correction_patterns)
                    {
                        scored.push((
                            strength,
                            UserCorrectionEntry {
                                timestamp: entry.timestamp().map(|t| t.to_rfc3339()),
                                user_text: truncate_text(&text, opts.correction_text_len),
                                prior_assistant_summary: Some(prior_assistant_summary),
                                correction_basis,
                            },
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    // Stable sort by strength descending: stronger corrections first, with
    // equal-strength candidates keeping their chronological order.
    scored.sort_by_key(|(strength, _)| std::cmp::Reverse(*strength));
    scored.into_iter().map(|(_, c)| c).collect()
}

/// Rank tools by error frequency.
///
/// Returns a sorted list of `(tool_name, error_count)` pairs, most error-prone first.
pub fn rank_error_prone_tools(pairs: &[ErrorFixPair]) -> Vec<(String, usize)> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for pair in pairs {
        *counts.entry(pair.tool_name.clone()).or_default() += 1;
    }
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by_key(|b| std::cmp::Reverse(b.1));
    ranked
}

/// Extract all lessons from a chronological entry list.
///
/// This is the main entry point. It combines error→fix pair extraction,
/// user correction detection, and summary statistics.
///
/// The summary reports **true totals** across the entire session, while
/// the returned vectors are truncated to `opts.limit`.
pub fn extract_lessons(entries: &[&LogEntry], opts: &LessonOptions) -> LessonResult {
    extract_lessons_with(entries, opts, None, is_human_prompt)
}

fn extract_lessons_with<Human>(
    entries: &[&LogEntry],
    opts: &LessonOptions,
    semantic: Option<&SemanticLessonContext>,
    is_human: Human,
) -> LessonResult
where
    Human: Fn(&LogEntry) -> bool,
{
    // Compute session-wide facts before applying the category projection. A
    // request for only corrections must not turn real failures into zeroes in
    // the summary (and vice versa).
    let all_error_fix_pairs = extract_error_fix_pairs_with(entries, opts, semantic);
    let all_user_corrections = extract_user_corrections_with(entries, opts, is_human);

    // Summary reflects true totals (ranked from all errors, not just the limited set)
    let total_errors = all_error_fix_pairs.len();
    let confirmed_tool_failures = all_error_fix_pairs
        .iter()
        .filter(|pair| pair.failure_kind == FailureKind::Confirmed)
        .count();
    let inferred_failure_signals = total_errors - confirmed_tool_failures;
    let total_corrections = all_user_corrections.len();
    let most_error_prone_tools = rank_error_prone_tools(&all_error_fix_pairs);

    let mut error_fix_pairs = if opts.category == LessonCategory::Corrections {
        Vec::new()
    } else {
        all_error_fix_pairs
    };
    let mut user_corrections = if opts.category == LessonCategory::Errors {
        Vec::new()
    } else {
        all_user_corrections
    };

    // Truncate returned vectors to the requested limit
    error_fix_pairs.truncate(opts.limit);
    user_corrections.truncate(opts.limit);

    LessonResult {
        summary: LessonsSummary {
            total_errors,
            confirmed_tool_failures,
            inferred_failure_signals,
            total_corrections,
            most_error_prone_tools,
        },
        error_fix_pairs,
        user_corrections,
    }
}

/// Extract lessons from a complete conversation under the provider's declared
/// semantic capability.
///
/// The capability is explicit: bundle presence alone is not semantic coverage
/// (the Claude adapter retains a complete bundle but deliberately uses classic
/// prompt/tool heuristics).
#[must_use]
pub fn extract_lessons_from_conversation(
    conversation: &Conversation,
    opts: &LessonOptions,
    semantic_annotations: bool,
) -> LessonResult {
    let all_entries = conversation.chronological_entries();
    if !semantic_annotations {
        return extract_lessons(&all_entries, opts);
    }
    let Some(bundle) = conversation.provider_bundle() else {
        return extract_lessons(&all_entries, opts);
    };

    let entries: Vec<&LogEntry> = all_entries
        .into_iter()
        .filter(|entry| {
            entry
                .uuid()
                .and_then(|uuid| conversation.semantics_for_uuid(uuid))
                .is_none_or(|semantics| semantics.activity == ActivityKind::New)
        })
        .collect();
    let mut semantic = SemanticLessonContext::default();
    for entry_semantics in bundle.semantics.values() {
        for (call_id, tool) in &entry_semantics.tools {
            semantic
                .tools
                .entry(call_id.clone())
                .or_insert_with(|| tool.clone());
        }
    }
    let is_human = |entry: &LogEntry| {
        matches!(entry, LogEntry::User(_))
            && entry
                .uuid()
                .and_then(|uuid| conversation.semantics_for_uuid(uuid))
                .and_then(|semantics| semantics.prompt)
                .is_some_and(|prompt| prompt.authorship == PromptAuthorship::Human)
    };
    extract_lessons_with(&entries, opts, Some(&semantic), is_human)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn semantics(kind: ToolKind, native_name: &str) -> ToolSemantics {
        ToolSemantics {
            kind,
            native_name: native_name.to_string(),
            lifecycle: Vec::new(),
        }
    }

    fn lifecycle_semantics(
        kind: ToolKind,
        status: crate::provider::ToolExecutionStatus,
        success: Option<bool>,
        exit_code: Option<i32>,
    ) -> ToolSemantics {
        ToolSemantics {
            kind,
            native_name: "native".into(),
            lifecycle: vec![crate::provider::ToolLifecycleObservation {
                record: crate::provider::RecordRef {
                    artifact: crate::provider::ArtifactId {
                        provider_instance: "test".into(),
                        locator: "fixture".into(),
                    },
                    ordinal: 7,
                },
                kind: crate::provider::ToolLifecycleKind::Command,
                status: Some(status),
                success,
                exit_code,
                duration: Some(std::time::Duration::from_millis(25)),
                source: Some("test".into()),
            }],
        }
    }

    #[test]
    fn native_lifecycle_status_is_authoritative_over_result_text() {
        let re = build_soft_error_regex().unwrap();
        let failed = lifecycle_semantics(
            ToolKind::FileWrite,
            crate::provider::ToolExecutionStatus::Failed,
            Some(false),
            None,
        );
        let failure = semantic_tool_result_failure(
            &failed,
            &serde_json::Value::Null,
            "looks harmless",
            Some(&re),
        )
        .unwrap();
        assert_eq!(failure.kind, FailureKind::Confirmed);
        assert_eq!(failure.basis, FailureBasis::NativeLifecycleStatus);

        let succeeded = lifecycle_semantics(
            ToolKind::Shell,
            crate::provider::ToolExecutionStatus::Completed,
            None,
            Some(0),
        );
        assert_eq!(
            semantic_tool_result_failure(
                &succeeded,
                &serde_json::json!({"cmd": "cat failure-fixture.txt"}),
                "panic: quoted source text",
                Some(&re),
            ),
            None,
            "confirmed success must suppress text-signature false positives"
        );
    }

    #[test]
    fn lifecycle_process_exit_preserves_no_match_exception() {
        let failed = lifecycle_semantics(
            ToolKind::Shell,
            crate::provider::ToolExecutionStatus::Failed,
            None,
            Some(1),
        );
        assert_eq!(
            lifecycle_verdict(&failed, &serde_json::json!({"cmd": "rg absent src"})),
            LifecycleVerdict::Succeeded
        );
        assert_eq!(
            lifecycle_verdict(&failed, &serde_json::json!({"cmd": "cargo test"})),
            LifecycleVerdict::Failed(FailureBasis::ProcessExit)
        );
    }

    #[test]
    fn semantic_shell_status_beats_error_words_in_successful_output() {
        let re = build_soft_error_regex().unwrap();
        let output = "Process exited with code 0\nFinal output:\nerror[E0308]: quoted fixture";
        assert!(!semantic_tool_result_is_error(
            &semantics(ToolKind::Shell, "exec_command"),
            &serde_json::json!({"cmd": "cargo check"}),
            output,
            Some(&re),
        ));
    }

    #[test]
    fn semantic_shell_nonzero_is_an_error_but_running_and_no_match_are_not() {
        let re = build_soft_error_regex().unwrap();
        let shell = semantics(ToolKind::Shell, "exec_command");
        assert!(semantic_tool_result_is_error(
            &shell,
            &serde_json::json!({"cmd": "cargo test"}),
            "Process exited with code 101\nFinal output:\nfailed",
            Some(&re),
        ));
        assert!(!semantic_tool_result_is_error(
            &shell,
            &serde_json::json!({"cmd": "long task"}),
            "Process running with session ID 42\nLive output:\nstill working",
            Some(&re),
        ));
        assert!(!semantic_tool_result_is_error(
            &shell,
            &serde_json::json!({"cmd": "rg missing"}),
            "Process exited with code 1\nFinal output:\n",
            Some(&re),
        ));
    }

    #[test]
    fn semantic_content_tools_do_not_treat_source_text_as_failures() {
        let re = build_soft_error_regex().unwrap();
        assert!(!semantic_tool_result_is_error(
            &semantics(ToolKind::FileRead, "read_file"),
            &serde_json::Value::Null,
            "thread worker panicked in this test fixture",
            Some(&re),
        ));
        assert!(!semantic_tool_result_is_error(
            &semantics(ToolKind::Search, "grep"),
            &serde_json::Value::Null,
            "42:error[E0308] appears in source",
            Some(&re),
        ));
        assert!(!semantic_tool_result_is_error(
            &semantics(ToolKind::Orchestration, "exec"),
            &serde_json::Value::Null,
            r#"[{"type":"input_text","text":"Script completed\nOutput:\nerror[E0308] in source"}]"#,
            Some(&re),
        ));
        assert!(!semantic_tool_result_is_error(
            &semantics(ToolKind::Other("future_tool".into()), "future_tool"),
            &serde_json::Value::Null,
            "thread worker panicked in content of unknown meaning",
            Some(&re),
        ));
    }

    #[test]
    fn semantic_shell_reads_structured_and_text_exit_statuses() {
        let re = build_soft_error_regex().unwrap();
        let shell = semantics(ToolKind::Shell, "exec_command");
        assert!(!semantic_tool_result_is_error(
            &shell,
            &serde_json::json!({"cmd": "cargo check"}),
            r#"{"exit_code":0,"output":"error[E0308] quoted source"}"#,
            Some(&re),
        ));
        assert!(semantic_tool_result_is_error(
            &shell,
            &serde_json::json!({"cmd": "cargo check"}),
            r#"{"chunk_id":"x","exit_code":101,"output":"compile failed"}"#,
            Some(&re),
        ));
        assert!(semantic_tool_result_is_error(
            &shell,
            &serde_json::json!({"cmd": "cargo check"}),
            "Exit code: 2\nFinal output:\ncompile failed",
            Some(&re),
        ));
    }

    #[test]
    fn semantic_patch_and_structured_mcp_failures_are_detected() {
        let re = build_soft_error_regex().unwrap();
        assert!(semantic_tool_result_is_error(
            &semantics(ToolKind::FileWrite, "apply_patch"),
            &serde_json::Value::Null,
            "apply_patch verification failed: Failed to find expected lines",
            Some(&re),
        ));
        assert!(semantic_tool_result_is_error(
            &semantics(ToolKind::Mcp, "mcp__tool"),
            &serde_json::Value::Null,
            r#"{"error":{"code":"failed"}}"#,
            Some(&re),
        ));
    }

    #[test]
    fn missing_provider_tool_semantics_does_not_reenable_classic_soft_matching() {
        let entry: LogEntry = serde_json::from_value(serde_json::json!({
            "type": "user", "uuid": "u1", "parentUuid": null,
            "timestamp": "2026-01-01T00:00:00Z", "sessionId": "s", "version": "1",
            "message": {"role": "user", "content": [{"type": "tool_result",
                "tool_use_id": "missing", "content": "error[E0308] quoted source"}]}
        }))
        .unwrap();
        let refs = vec![&entry];
        let result = extract_error_fix_pairs_with(
            &refs,
            &LessonOptions::default(),
            Some(&SemanticLessonContext::default()),
        );
        assert!(result.is_empty());
    }

    /// Helper: build a User entry that contains a tool result.
    fn user_tool_result(tool_use_id: &str, is_error: bool, content: &str) -> LogEntry {
        let is_error_str = if is_error { "true" } else { "false" };
        let json = format!(
            r#"{{
                "type": "user",
                "uuid": "user-{tool_use_id}",
                "timestamp": "2025-01-01T00:00:00Z",
                "sessionId": "test-session",
                "version": "2.0.0",
                "message": {{
                    "role": "user",
                    "content": [
                        {{
                            "type": "tool_result",
                            "tool_use_id": "{tool_use_id}",
                            "content": "{content}",
                            "is_error": {is_error_str}
                        }}
                    ]
                }}
            }}"#
        );
        serde_json::from_str(&json).expect("failed to parse user_tool_result JSON")
    }

    /// Helper: build an Assistant entry with a tool use and text.
    fn assistant_with_tool(tool_id: &str, tool_name: &str, text: &str) -> LogEntry {
        let json = format!(
            r#"{{
                "type": "assistant",
                "uuid": "asst-{tool_id}",
                "timestamp": "2025-01-01T00:00:01Z",
                "sessionId": "test-session",
                "version": "2.0.0",
                "message": {{
                    "id": "msg-test",
                    "type": "message",
                    "role": "assistant",
                    "model": "claude-sonnet-4-20250514",
                    "content": [
                        {{
                            "type": "tool_use",
                            "id": "{tool_id}",
                            "name": "{tool_name}",
                            "input": {{"command": "cargo test"}}
                        }},
                        {{
                            "type": "text",
                            "text": "{text}"
                        }}
                    ],
                    "stop_reason": "end_turn"
                }}
            }}"#
        );
        serde_json::from_str(&json).expect("failed to parse assistant_with_tool JSON")
    }

    /// Helper: build a simple assistant text entry.
    fn assistant_text(text: &str) -> LogEntry {
        serde_json::from_value(serde_json::json!({
            "type": "assistant",
            "uuid": "asst-text",
            "timestamp": "2025-01-01T00:00:02Z",
            "sessionId": "test-session",
            "version": "2.0.0",
            "message": {
                "id": "msg-test",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-4-20250514",
                "content": [{"type": "text", "text": text}],
                "stop_reason": "end_turn"
            }
        }))
        .expect("failed to parse assistant_text JSON")
    }

    /// Helper: build a simple user text entry.
    fn user_text(text: &str) -> LogEntry {
        serde_json::from_value(serde_json::json!({
            "type": "user",
            "uuid": "user-text",
            "timestamp": "2025-01-01T00:00:03Z",
            "sessionId": "test-session",
            "version": "2.0.0",
            "message": {"role": "user", "content": text}
        }))
        .expect("failed to parse user_text JSON")
    }

    #[test]
    fn test_extract_hard_error_fix_pair() {
        let entries = [
            assistant_with_tool("t1", "Bash", "Running tests"),
            user_tool_result("t1", true, "error: test failed"),
            assistant_text("I see the test failed. Let me fix the issue."),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let pairs = extract_error_fix_pairs(&refs, &opts);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].tool_name, "Bash");
        assert!(pairs[0].error_preview.contains("test failed"));
        assert!(pairs[0].resolution_summary.is_some());
        assert!(pairs[0]
            .resolution_summary
            .as_ref()
            .unwrap()
            .contains("fix the issue"));
    }

    #[test]
    fn test_extract_soft_error() {
        let entries = [
            assistant_with_tool("t1", "Bash", "Building"),
            user_tool_result("t1", false, "error[E0308]: mismatched types"),
            assistant_text("The compiler error indicates a type mismatch."),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let pairs = extract_error_fix_pairs(&refs, &opts);
        assert_eq!(pairs.len(), 1);
        assert!(pairs[0].error_preview.contains("E0308"));
    }

    #[test]
    fn test_no_error_no_pair() {
        let entries = [
            assistant_with_tool("t1", "Bash", "Running"),
            user_tool_result("t1", false, "All 5 tests passed"),
            assistant_text("Tests passed!"),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let pairs = extract_error_fix_pairs(&refs, &opts);
        assert!(pairs.is_empty());
    }

    #[test]
    fn test_error_limit_respected() {
        let mut entries = Vec::new();
        for i in 0..10 {
            let id = format!("t{i}");
            entries.push(assistant_with_tool(&id, "Bash", ""));
            entries.push(user_tool_result(&id, true, "error"));
            entries.push(assistant_text("fixed"));
        }
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions {
            limit: 3,
            ..Default::default()
        };

        let result = extract_lessons(&refs, &opts);
        // Returned items are limited
        assert_eq!(result.error_fix_pairs.len(), 3);
        // But summary reflects true total
        assert_eq!(result.summary.total_errors, 10);
    }

    #[test]
    fn test_extract_user_correction() {
        let entries = [
            assistant_text("I'll delete the file now."),
            user_text("No, don't delete that file!"),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let corrections = extract_user_corrections(&refs, &opts);
        assert_eq!(corrections.len(), 1);
        assert!(corrections[0].user_text.contains("don't delete"));
        assert!(corrections[0].prior_assistant_summary.is_some());
        assert_eq!(
            corrections[0].correction_basis,
            CorrectionBasis::ExplicitRejection
        );
    }

    fn user_meta(text: &str) -> LogEntry {
        let json = format!(
            r#"{{
                "type": "user",
                "uuid": "user-meta",
                "isMeta": true,
                "timestamp": "2025-01-01T00:00:04Z",
                "sessionId": "test-session",
                "version": "2.0.0",
                "message": {{ "role": "user", "content": "{text}" }}
            }}"#
        );
        serde_json::from_str(&json).expect("failed to parse user_meta JSON")
    }

    #[test]
    fn correction_classification_and_ranking_share_one_signal_set() {
        let entries = [
            assistant_text("Working on it."),
            // Ordinary collaboration containing the old standalone "again"
            // marker is not a correction.
            user_text("Please run the tests again, thanks for the help here."),
            assistant_text("Sure."),
            // isMeta entry containing correction words must be excluded entirely.
            user_meta("no, that is wrong, stop"),
            assistant_text("OK."),
            // Structural redirect with no old ranking marker is retained.
            user_text("Continue the previous request exactly as written."),
            assistant_text("Continuing."),
            // Explicit rejection ranks before the redirect.
            user_text("No, that's wrong. Stop doing that, do it the other way instead."),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();
        let corrections = extract_user_corrections(&refs, &opts);

        assert_eq!(
            corrections.len(),
            2,
            "ordinary 'again' and meta content are excluded: {corrections:?}"
        );
        assert_eq!(
            corrections[0].correction_basis,
            CorrectionBasis::ExplicitRejection
        );
        assert_eq!(
            corrections[1].correction_basis,
            CorrectionBasis::IntentClarification
        );
    }

    #[test]
    fn ordinary_corpus_phrases_are_not_corrections() {
        let entries = [
            // No preceding assistant: an initial negative task constraint is
            // not a correction of agent behavior.
            user_text("Do NOT write any files; return findings as your final response."),
            assistant_text("What would you like me to do?"),
            user_text("Is this captured so we don't leave anything on the table?"),
            assistant_text("Yes."),
            user_text("Some of these findings may already be in context."),
            assistant_text("Understood."),
            user_text("One small note for later. No action needed."),
            assistant_text("Thanks."),
            user_text("I restarted Claude Code again and the server reconnected."),
            assistant_text("Good."),
            user_text("Demystify what is already complete and what remains."),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let corrections = extract_user_corrections(&refs, &LessonOptions::default());
        assert!(
            corrections.is_empty(),
            "ordinary uses of don't/already/again/no are not dialogue repair: {corrections:?}"
        );
    }

    #[test]
    fn corpus_shaped_dialogue_repairs_keep_their_basis() {
        let entries = [
            assistant_text("I will continue with a different task."),
            user_text("Continue the previous B3.1.3 request exactly as written."),
            assistant_text("Everything is fine."),
            user_text("This other agent keeps raising issues with your work after every round."),
            assistant_text("I chose not to read the conversation."),
            user_text("Wait. Why did you not read through the conversation content?"),
            assistant_text("You asked for a single artifact."),
            user_text("What I really meant was a system of related artifacts."),
            assistant_text("Here is the technical terminology."),
            user_text("I don't understand your verbiage."),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let corrections = extract_user_corrections(&refs, &LessonOptions::default());
        let bases: std::collections::BTreeSet<_> = corrections
            .iter()
            .map(|correction| correction.correction_basis.as_str())
            .collect();
        assert_eq!(
            corrections.len(),
            5,
            "all observed repair shapes survive: {corrections:?}"
        );
        assert!(bases.contains("intent clarification"));
        assert!(bases.contains("performance critique"));
    }

    #[test]
    fn test_correction_skips_harness_boilerplate() {
        // #26: harness-injected/templated content that happens to contain
        // correction words (command echoes, local-command-stdout hook output,
        // system-reminder blocks) must not be extracted as user corrections,
        // while a genuine correction in the same run still is.
        let entries = [
            assistant_text("Doing the thing."),
            user_text("<local-command-stdout>goal: stop the wrong build</local-command-stdout>"),
            user_text("<command-name>/goal</command-name>"),
            user_text("<system-reminder>no, actually do X instead</system-reminder>"),
            assistant_text("I'll delete the file."),
            user_text("No, don't delete that, it is wrong."),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let corrections = extract_user_corrections(&refs, &opts);
        assert_eq!(
            corrections.len(),
            1,
            "only the genuine correction should survive, got {corrections:?}"
        );
        assert!(corrections[0].user_text.contains("don't delete"));
    }

    #[test]
    fn relayed_review_does_not_become_the_users_correction() {
        let entries = [
            assistant_text("Here is the current implementation."),
            user_text(
                "I shared the work with another reviewer. Their response follows:\n\
                 ```\nThis is wrong and you should have done it another way. Stop repeating it.\n```\n\
                 Please investigate their findings.",
            ),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let corrections = extract_user_corrections(&refs, &LessonOptions::default());
        assert!(
            corrections.is_empty(),
            "quoted reviewer language is not the user's own correction: {corrections:?}"
        );
    }

    #[test]
    fn primary_correction_survives_beside_a_quoted_review() {
        let entries = [
            assistant_text("Here is the current implementation."),
            user_text(
                "No, stop claiming this is complete.\n```\nThe reviewer also said it is wrong.\n```",
            ),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let corrections = extract_user_corrections(&refs, &LessonOptions::default());
        assert_eq!(corrections.len(), 1);
        assert!(corrections[0].user_text.contains("stop claiming"));
        assert!(!corrections[0].user_text.contains("reviewer also"));
    }

    #[test]
    fn test_correction_skips_short_text() {
        let entries = [
            assistant_text("working on it"),
            user_text("no"), // too short (<=10 chars)
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let corrections = extract_user_corrections(&refs, &opts);
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_correction_skips_summaries() {
        let long_summary = "This session is being continued from a previous conversation that ran out of context. ".repeat(50);
        let entries = [assistant_text("working"), user_text(&long_summary)];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let corrections = extract_user_corrections(&refs, &opts);
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_category_filter_errors_only() {
        let entries = [
            assistant_with_tool("t1", "Bash", ""),
            user_tool_result("t1", true, "error"),
            assistant_text("fixed"),
            user_text("Why did you do that wrong thing again?"),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions {
            category: LessonCategory::Errors,
            ..Default::default()
        };

        let result = extract_lessons(&refs, &opts);
        assert_eq!(result.error_fix_pairs.len(), 1);
        assert!(result.user_corrections.is_empty());
        assert_eq!(result.summary.total_errors, 1);
        assert_eq!(result.summary.total_corrections, 1);
    }

    #[test]
    fn test_category_filter_corrections_only() {
        let entries = [
            assistant_with_tool("t1", "Bash", ""),
            user_tool_result("t1", true, "error"),
            assistant_text("I tried something wrong."),
            user_text("That's not what I asked for, stop doing that."),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions {
            category: LessonCategory::Corrections,
            ..Default::default()
        };

        let result = extract_lessons(&refs, &opts);
        assert!(result.error_fix_pairs.is_empty());
        assert_eq!(result.user_corrections.len(), 1);
        assert_eq!(result.summary.total_errors, 1);
        assert_eq!(result.summary.confirmed_tool_failures, 1);
        assert_eq!(result.summary.total_corrections, 1);
    }

    #[test]
    fn test_rank_error_prone_tools() {
        let pairs = vec![
            ErrorFixPair {
                timestamp: None,
                tool_name: "Bash".into(),
                input_summary: HashMap::new(),
                error_preview: "err".into(),
                failure_kind: FailureKind::Confirmed,
                failure_basis: FailureBasis::NativeErrorFlag,
                resolution_summary: None,
                resolution_tools: vec![],
            },
            ErrorFixPair {
                timestamp: None,
                tool_name: "Edit".into(),
                input_summary: HashMap::new(),
                error_preview: "err".into(),
                failure_kind: FailureKind::Confirmed,
                failure_basis: FailureBasis::NativeErrorFlag,
                resolution_summary: None,
                resolution_tools: vec![],
            },
            ErrorFixPair {
                timestamp: None,
                tool_name: "Bash".into(),
                input_summary: HashMap::new(),
                error_preview: "err".into(),
                failure_kind: FailureKind::Confirmed,
                failure_basis: FailureBasis::NativeErrorFlag,
                resolution_summary: None,
                resolution_tools: vec![],
            },
        ];

        let ranked = rank_error_prone_tools(&pairs);
        assert_eq!(ranked[0], ("Bash".to_string(), 2));
        assert_eq!(ranked[1], ("Edit".to_string(), 1));
    }

    #[test]
    fn test_extract_lessons_full() {
        let entries = [
            assistant_with_tool("t1", "Bash", "Running cargo test"),
            user_tool_result("t1", true, "error: compilation failed"),
            assistant_text("I see the compilation error. Let me fix it."),
            user_text("Why did you run tests before fixing the import?"),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let result = extract_lessons(&refs, &opts);
        assert_eq!(result.error_fix_pairs.len(), 1);
        assert_eq!(result.user_corrections.len(), 1);
        assert_eq!(result.summary.total_errors, 1);
        assert_eq!(result.summary.confirmed_tool_failures, 1);
        assert_eq!(result.summary.inferred_failure_signals, 0);
        assert_eq!(result.summary.total_corrections, 1);
        assert!(!result.summary.most_error_prone_tools.is_empty());
    }

    #[test]
    fn shared_failure_taxonomy_separates_confirmed_from_inferred() {
        let entries = [
            assistant_with_tool("hard", "Bash", "run hard"),
            user_tool_result("hard", true, "native failure"),
            assistant_with_tool("soft", "Bash", "run soft"),
            user_tool_result("soft", false, "error[E0308]: inferred compiler output"),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let result = extract_lessons(&refs, &LessonOptions::default());
        assert_eq!(result.summary.total_errors, 2);
        assert_eq!(result.summary.confirmed_tool_failures, 1);
        assert_eq!(result.summary.inferred_failure_signals, 1);
        assert_eq!(
            result.error_fix_pairs[0].failure_kind,
            FailureKind::Confirmed
        );
        assert_eq!(
            result.error_fix_pairs[1].failure_kind,
            FailureKind::Inferred
        );
    }

    #[test]
    fn test_false_positive_read_file_content() {
        // Read tool result with is_error=true but content is valid file output
        let entries = [
            assistant_with_tool("t1", "Read", "Reading file"),
            user_tool_result("t1", true, "     1\\u2192fn main() {}"),
            assistant_text("Got the file."),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let pairs = extract_error_fix_pairs(&refs, &opts);
        assert!(
            pairs.is_empty(),
            "Read with line-numbered content should be filtered as false positive"
        );
    }

    #[test]
    fn test_false_positive_mcp_json_result() {
        // MCP tool result with is_error=true but content is valid JSON
        let entries = [
            assistant_with_tool("t1", "mcp__snatch__get_session_lessons", "Fetching lessons"),
            user_tool_result("t1", true, "{session_id: abc}"),
            assistant_text("Got the lessons."),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let pairs = extract_error_fix_pairs(&refs, &opts);
        assert!(
            pairs.is_empty(),
            "MCP tool returning JSON should be filtered as false positive"
        );
    }

    #[test]
    fn test_false_positive_read_offset_content() {
        // Read tool result with is_error=true but content is file output at offset
        let entries = [
            assistant_with_tool("t1", "Read", "Reading file"),
            user_tool_result("t1", true, "787\\u2192#[cfg(test)]"),
            assistant_text("Got the file."),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let pairs = extract_error_fix_pairs(&refs, &opts);
        assert!(
            pairs.is_empty(),
            "Read with offset line-numbered content should be filtered"
        );
    }

    #[test]
    fn test_false_positive_grep_output() {
        // Grep tool result with is_error=true but content is grep output
        let entries = [
            assistant_with_tool("t1", "Grep", "Searching"),
            user_tool_result("t1", true, "21:[Omitted long matching line]"),
            assistant_text("Found matches."),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let pairs = extract_error_fix_pairs(&refs, &opts);
        assert!(
            pairs.is_empty(),
            "Grep with line-numbered output should be filtered"
        );
    }

    #[test]
    fn test_real_read_error_not_filtered() {
        // Read tool result with is_error=true and actual error content
        let entries = [
            assistant_with_tool("t1", "Read", "Reading file"),
            user_tool_result("t1", true, "File does not exist."),
            assistant_text("The file is missing."),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let pairs = extract_error_fix_pairs(&refs, &opts);
        assert_eq!(pairs.len(), 1, "Real Read error should NOT be filtered");
    }

    #[test]
    fn test_is_likely_false_positive() {
        // Read: line-numbered content at any offset
        assert!(is_likely_false_positive("Read", "     1→fn main() {}"));
        assert!(is_likely_false_positive("Read", "1→hello"));
        assert!(is_likely_false_positive("Read", "787→#[cfg(test)]"));
        assert!(is_likely_false_positive("Read", "   42→some line"));
        // Read: tab-delimited line numbers (current cat -n format)
        assert!(is_likely_false_positive("Read", "1\t//! module doc"));
        assert!(is_likely_false_positive("Read", "595\t    ] {"));
        assert!(is_likely_false_positive("Read", "     42\tsome line"));
        assert!(!is_likely_false_positive("Read", "File does not exist."));
        assert!(!is_likely_false_positive(
            "Read",
            "Sibling tool call errored"
        ));

        // Grep: grep-style line output
        assert!(is_likely_false_positive(
            "Grep",
            "21:[Omitted long matching line]"
        ));
        assert!(is_likely_false_positive("Grep", "5-context line"));
        assert!(!is_likely_false_positive(
            "Grep",
            "InputValidationError: Grep failed"
        ));

        // MCP: JSON responses
        assert!(is_likely_false_positive(
            "mcp__snatch__list_sessions",
            r#"{"sessions": []}"#
        ));
        assert!(!is_likely_false_positive(
            "mcp__snatch__list_sessions",
            "MCP error -32000: Connection closed"
        ));

        // Agent: substantial non-error text
        assert!(is_likely_false_positive("Agent", &"x".repeat(300)));
        assert!(!is_likely_false_positive(
            "Agent",
            "Error: something went wrong"
        ));

        // Other tools: no filtering
        assert!(!is_likely_false_positive(
            "Bash",
            "error: compilation failed"
        ));
    }
}
