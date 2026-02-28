# Compaction Archaeology: Failure Mode Taxonomy & Research Process

## Overview

This document records the systematic analysis of Claude Code's auto-compaction behavior and its impact on session continuity. The research was conducted using `snatch` MCP tools to analyze real session data across multiple projects.

## Failure Mode Taxonomy

Six distinct failure modes were identified from analyzing 157 compaction events across 24 sessions, with 7 detailed compaction boundary analyses.

### F1: Evidence Evaporation (Critical)

**Frequency:** Every compaction
**Description:** The chain of evidence that proves a conclusion is reduced to just the conclusion. GDB traces, IR dumps, step-by-step debugging — all compressed to "found bug X in file Y."

**Example:** Original: "rsi=0x0 at offset 0x4a2, parse_block constructs `{i32, {i64,i64,i32,i32}}` but lower_block reads `{ptr,i64,i64}`" → After compaction: "struct field ordering bug in parse_block"

**Recoverability:** Partially recoverable. Thinking blocks (100% lost in compaction) ARE stored in JSONL and contain reasoning chains. Tool I/O (also lost) contains the raw evidence.

### F2: Negative Result Amnesia (Critical)

**Frequency:** Every compaction
**Description:** What was tried and failed is almost never preserved. The agent retries approaches that already failed, wasting entire compaction windows.

**Example:** In the `blood` session, "tee hangs in Claude's tool environment" was learned and forgotten 3 times. `eprint_u64` doesn't exist in Blood was discovered twice. The `alloca {}` investigation consumed an entire compaction window before being eliminated as a red herring.

**Recoverability:** Recoverable via error→fix pattern detection in tool calls and search for negative language patterns.

### F3: Decision Rationale Loss (High)

**Frequency:** ~80% of compactions
**Description:** WHY a particular approach was chosen is lost. The summary preserves WHAT was decided but not the alternatives considered, tradeoffs evaluated, or evidence that supported the choice.

**Example:** 90K chars of architectural reasoning (reviewer feedback, author response, convergence document) compressed to "decided to use approach X" in the cartograph session. The first compaction boundary had 9.5x compression.

**Recoverability:** Recoverable via thinking block retrieval and search for decision-related language.

### F4: Operational Gotcha Amnesia (High)

**Frequency:** ~60% of compactions
**Description:** Environmental and operational knowledge (build system quirks, tool limitations, workarounds) is repeatedly lost and relearned.

**Example:** Build system behavior (ground-truth defaults to first_gen, not second_gen) relearned 3+ times in a single blood session across compaction boundaries.

**Recoverability:** Recoverable via error→fix pair extraction and lesson pattern detection.

### F5: User Document Compression (High)

**Frequency:** Whenever users paste large documents
**Description:** User-pasted documents (specifications, analysis, feedback) are compressed to 1-3 sentence descriptions. The cartograph session lost 90K chars of user text in a single compaction (9.5x compression).

**Recoverability:** Fully recoverable — original user messages are in JSONL.

### F6: Summary Bloat Spiral (Medium)

**Frequency:** Heavy sessions (10+ compactions)
**Description:** Each compaction summary consumes 20-30% of the context window, leaving less room for actual work, causing faster compaction, requiring more summaries. This creates a vicious cycle in long sessions.

**Example:** The blood session with 33 compaction events over 31 hours. By compaction #15, summaries were 400-500 lines each, consuming significant context just to re-establish known state.

**Recoverability:** Mitigated by targeted retrieval (retrieve only what's needed) rather than comprehensive recovery.

## Compression Ratio Patterns

| Session Type | Compression | Loss Severity |
|---|---|---|
| Heavy tool-use, sparse dialogue | ~1x | Low — summary captures narrative well |
| Mechanical/repetitive work | 0.9-1.1x | Minimal — original was already terse |
| Deliberative/collaborative discussion | 3-10x | Severe — rich reasoning compressed to bullets |

## Key Statistics from Initial Collection

| Metric | Value |
|---|---|
| Total sessions examined | 120 main sessions |
| Sessions with compaction | 24 (20%) |
| Total compaction events | 157 |
| Compaction rate threshold | ~2-3 hours of active use |
| Average interval between compactions | 20-60 minutes |
| Most compacted session | 33 events over 31 hours (blood) |
| Highest single-boundary compression | 9.5x (cartograph) |
| Thinking block loss rate | 100% (always completely dropped) |
| Thinking blocks in JSONL | Present and recoverable |

## What Compaction Preserves Well

- Chronological narrative structure
- File paths and commit hashes
- What user prompts were asked (verbatim quotes)
- Which tools were used (names only)
- High-level outcomes and decisions

## What Compaction Never Preserves

- Thinking blocks (100% loss, verified across all sessions)
- Tool I/O content (code written/read, command output)
- Evidence chains (GDB traces, IR dumps, debugging steps)
- Failed approaches and negative results
- Emotional calibration and user frustration signals
- Intermediate states between fixes

## Detected Relearning Patterns

Relearning (agent re-discovers something it already knew pre-compaction) is the most expensive consequence. Examples from the blood session:

1. Build script behavior — relearned 3+ times
2. Language builtins (`eprint_u64` doesn't exist) — discovered twice
3. Red herring investigations (`alloca {}`) — consumed full compaction window
4. User analysis re-evaluation — user's 7-point analysis re-evaluated from scratch 3 times
5. ABI conventions — re-discovered at later compaction points

---

## Archaeological Process

### How to reproduce this analysis

This process can be run at any time to collect more data and refine the failure mode taxonomy.

### Step 1: Survey the landscape

```
list_sessions(limit=200, include_subagents=false)
```

Get all main sessions. Note total count and project distribution.

### Step 2: Identify compaction-heavy sessions

```
get_project_history(project="<name>", period="all")
```

For each project, get session overview. Sessions with high token counts and long durations are likely to have compaction events.

### Step 3: Find compaction events

```
get_session_timeline(session_id="<id>", limit=200)
```

The timeline response includes a `compaction_events` array. Record the count and timestamps.

### Step 4: Analyze compaction boundaries

For each compaction event, examine the messages before and after:

```
# Get messages around the compaction boundary
get_session_messages(session_id="<id>", detail="full", limit=50, offset=<boundary_index - 25>)
```

Look for:
- What was the conversation about at the boundary?
- What specific information appears in pre-compaction messages that doesn't appear in post-compaction context?
- Are there thinking blocks with decision rationale?
- Are there error→fix sequences?

### Step 5: Search for relearning patterns

```
# Search for error patterns that recur
search_sessions(session_id="<id>", pattern="error|failed|crash|SIGSEGV", scope="all")

# Search for "lesson learned" language
search_sessions(session_id="<id>", pattern="don't|avoid|gotcha|mistake|wrong|already tried")

# Search for repeated investigations
search_sessions(session_id="<id>", pattern="<specific_topic>", scope="all")
```

Count how many times the same concept appears across compaction boundaries.

### Step 6: Measure compression ratios

For each compaction boundary:
- Count chars of original user text, assistant text, thinking blocks
- Count chars of the compaction summary
- Calculate ratio and categorize the session type

### Step 7: Classify and catalog

For each identified loss:
1. Assign to failure mode (F1-F6)
2. Note the severity (was it actually harmful?)
3. Note whether it's recoverable via snatch tools
4. Add to the taxonomy if it reveals a new pattern

### Automation opportunity

The manual process above could be partially automated with a dedicated snatch tool (e.g., `analyze_compaction_boundaries`) that:
- Finds all compaction events in a session
- Extracts pre/post content at each boundary
- Computes compression ratios
- Detects relearning patterns (same search terms appearing post-compaction)

---

## Revision History

- **2026-02-28**: Initial taxonomy from analysis of 157 compaction events across 24 sessions
  - Projects analyzed: blood (16 sessions), references (6), claude-snatch (1), cartograph (1), nostalgia (2)
  - 7 detailed compaction boundary analyses performed
  - 6 failure modes identified and classified
