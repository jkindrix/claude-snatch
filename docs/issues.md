# Snatch Issue Tracker

Consolidated from self-testing on claude-snatch and external agent testing on blood repo.

## Status Key

- `[ ]` Open
- `[x]` Fixed
- `[-]` Won't fix / Deferred

---

## High Priority

### 1. Project disambiguation broken

**Source:** Blood agent
**Category:** CLI / discovery
**Blocked:** decisions, conflicts, detect --register all unusable on blood

`-p blood` matches 8 projects (blood, blood/analysis, blood/docs, etc.). No way to specify exact path match. Every snatch command with `-p` fails until user finds a unique substring.

**Fix:** Add `--exact` flag or trailing boundary matching so `-p /home/jkindrix/blood` doesn't match `/home/jkindrix/blood/analysis`.

- [x] Implement exact project matching

---

### 2. `--register` produces garbage titles

**Source:** Self-testing
**Category:** detect / registry pipeline

Registered 13 "decisions" that were session continuation summaries, task notifications, and raw XML tags. The title comes from the user prompt text, not a decision summary.

**Fix:** Use assistant response text (truncated, first sentence or line) as the title instead of the user prompt. Skip candidates where question text looks like a session continuation or task notification.

- [x] Fix title extraction in register pipeline
- [x] Filter out session continuation / task notification prompts

---

### 3. Structural detection finds almost nothing

**Source:** Self-testing
**Category:** detect / heuristics

Only 1 structural match across all snatch sessions at any confidence. The `has_options_pattern` precision tuning requiring deliberation language alongside numbered/bullet lists overcorrected — real design conversations rarely use clean "Option A / Option B" format.

**Fix:** Relax the heuristic. Options: require deliberation language OR >= 3 numbered items (not both). Or lower the bar for numbered items when the preceding message is interrogative.

- [x] Relax `has_options_pattern` to reduce false negatives

---

### 4. Explicit marker "decided to" too broad

**Source:** Self-testing
**Category:** detect / heuristics

"decided to" appears in many non-decision contexts ("I decided to read the file first"). The marker fires on assistant messages, then grabs the previous user message as the "question" — which might be a session continuation summary or completely unrelated.

**Fix:** Add context filtering — skip markers in assistant messages where the previous user message is a continuation summary (starts with "This session is being continued" or similar). Consider requiring "decided to" to appear with a noun phrase indicating a design choice, not a trivial action.

- [x] Filter out continuation summaries from marker context
- [x] Consider tightening "decided to" pattern

---

## Medium Priority

### 5. Score title-word matching too loose

**Source:** Self-testing
**Category:** decisions score

A decision titled "Analysis functions take &[&LogEntry] not sessions" scored 40% because the word "not" triggered the correction pattern detector. Common words like "not", "use", "no" cause false matches.

**Fix:** Require >= 3 significant title words to match, skip words <= 3 chars, or use a stop-word list for matching.

- [x] Improve title-word matching precision

---

### 6. Score "skipped, no session_id" misleading

**Source:** Self-testing
**Category:** decisions score / UX

When session_id is set but `find_session` can't resolve it (e.g., full UUID format vs short prefix), the message says "skipped, no session_id" — confusing because the field IS set.

**Fix:** Differentiate "no session_id set" from "session_id set but session not found".

- [x] Improve error message for unresolvable session_id

---

### 7. Conflicts opposing-language heuristic too noisy

**Source:** Self-testing, blood agent
**Category:** conflicts / heuristics

At default confidence: "No conflicts detected" for most topics. At min-confidence 0.3: matches are just paragraphs containing incidental opposing words ("add"/"remove") in unrelated contexts. Blood agent confirms high false positive rate at 60-75% confidence.

**Fix:** Hard problem. Would need sentence-level context, not just word presence. Consider requiring opposing words to appear in the same sentence or within N words of each other, or requiring them to appear alongside the search topic terms.

- [ ] Improve opposing-language precision (approach TBD)

---

### 8. No UUID discovery in text search output

**Source:** Self-testing
**Category:** tagging / UX

Tagging requires a message UUID, but text output doesn't show UUIDs. User must switch to JSON output, parse it, extract the UUID, then run the tag command — 3-step process.

**Fix:** Add `--show-uuid` flag to search text output, or always show a short UUID prefix in results.

- [x] Add UUID display option to text search output

---

### 9. Phase classification meaningless for long sessions

**Source:** Self-testing
**Category:** search / phase context

"early, 953m in" (16 hours in but "early"). "middle, 5038m in" (84 hours, "middle"). The time-ratio approach breaks for sessions with huge time gaps and multiple compactions.

**Fix:** Use absolute thresholds (first 30m = early, last 30m = late), or compaction-boundary-based classification, or hybrid (ratio for short sessions, absolute for long).

- [x] Fix phase classification for long/mega sessions

---

### 10. `detect` gives empty results on date ranges

**Source:** Blood agent
**Category:** detect / temporal filters

`--since 2026-01-09 --until 2026-01-10` returns nothing even though decisions exist in genesis sessions. Session-specific `-s` works fine. Date range filtering may be comparing file modification time rather than session content timestamps.

**Fix:** Investigate whether date filtering uses file mtime vs message timestamps. May need to use session start/end time from analytics rather than file modification time.

- [ ] Diagnose and fix date range filtering in detect

---

### 11. Special characters in search patterns

**Source:** Blood agent
**Category:** search / CLI

`-> never` fails because `->` is parsed as a flag. Need `--` separator or quoting guidance.

**Fix:** Document `--` separator usage, or handle common special character patterns gracefully.

- [-] Standard `--` separator works; clap already suggests it in the error message

---

## Low Priority / Enhancements

### 12. Per-role `--max-context` for thread

**Source:** Blood agent
**Category:** thread / UX

`--max-context 500` is too short for design decisions that span paragraphs. When increased, output gets very large because user prompts (often continuation summaries) also expand.

**Wish:** `--max-context` could be per-role (e.g., show full assistant response but truncate user prompts).

- [x] Add `--max-user-context` / `--max-assistant-context` to thread

---

### 13. `snatch thread --summary`

**Source:** Blood agent
**Category:** thread / enhancement

After showing all exchanges chronologically, provide a one-paragraph synthesis of how the topic evolved. This is the manual step the agent keeps doing after every thread command.

**Note:** This would require either an LLM call (out of scope for a CLI tool) or a heuristic summary (first/last exchange + count). Consider a simple heuristic approach.

- [ ] Consider thread summary feature

---

### 14. Empty Q/A fields in detect candidates

**Source:** Self-testing
**Category:** detect / output quality

Many candidates have empty question or answer fields. These provide no value and clutter output.

**Fix:** Skip candidates where both question and response are empty.

- [x] Filter empty-field candidates from detect output

---

### 15. `--session` vs `--session-id` flag inconsistency

**Source:** Self-testing
**Category:** CLI / consistency

`decisions` uses `--session-id` while other commands use `--session` or `-s`.

**Fix:** Add `--session` as an alias for `--session-id`, or standardize across all commands.

- [x] Standardize session ID flag naming

---

## Round 2 — Blood Agent Feedback

### 16. `--register` title extraction broken for markdown-heavy responses

**Source:** Blood agent (mega sessions)
**Category:** detect / registry pipeline
**Priority:** High

`extract_decision_sentence` doesn't handle responses starting with `##`, `|` (tables), `**`, or `---`. Titles come out as table fragments or user prompt text. Needs to skip markdown formatting lines and find actual prose sentences.

- [x] Handle markdown headers, tables, bold, and horizontal rules in title extraction

---

### 17. Score can't follow decision provenance across continuation chains

**Source:** Blood agent
**Category:** decisions score
**Priority:** High

Scorer only checks the single linked session. If a decision was discussed across continuation chains (same conversation continued), confirmations in later segments are missed. Should follow continuation chains when scoring.

- [ ] Follow continuation chains when scoring decisions

---

### 18. No way to link a decision to multiple sessions

**Source:** Blood agent
**Category:** decisions / data model
**Priority:** High

Many decisions evolve across 3-5 sessions. Only one `--session` allowed. Need either multiple `--session` values or a `--related-sessions` field.

- [ ] Support multiple session references per decision

---

### 19. `detect --register` needs `--dry-run`

**Source:** Blood agent
**Category:** detect / UX
**Priority:** Medium

No way to preview what would be registered before committing. Users have to register, review, then delete. `--dry-run` would show titles/descriptions without writing.

- [x] Add `--dry-run` flag to detect --register

---

### 20. `decisions add` doesn't accept multiple tags

**Source:** Blood agent
**Category:** decisions / CLI
**Priority:** Medium

`--tag` only accepts one value. Most decisions span multiple domains. Need comma-separated or repeated `--tag` support.

- [x] Support multiple tags on decisions add

---

### 21. Thread output overwhelming for broad patterns — no decision-only filter

**Source:** Blood agent
**Category:** thread / filtering
**Priority:** Medium

Broad patterns return hundreds of exchanges. No way to filter to just decision-point exchanges. Wish: `--decisions-only` cross-references with detect heuristic.

- [ ] Consider `--decisions-only` filter for thread

---

### 22. detect date filtering returns wrong results

**Source:** Blood agent
**Category:** detect / temporal filters
**Priority:** Medium

`--since 2026-01-15 --until 2026-02-01` returned results from Jan 9. Related to issue #10 — mtime vs content timestamps. May also be that sessions span the boundary and the filter uses session-level not message-level timestamps.

- [ ] Investigate and fix (see also #10)

---

### 23. No search within decision descriptions

**Source:** Blood agent
**Category:** decisions / search
**Priority:** Medium

No way to check if a decision about "Send" already exists without visually scanning `decisions list`. Need `--search` or `--filter` on list.

- [x] Add text search/filter to decisions list

---

### 24. `decisions list` doesn't show session ID

**Source:** Blood agent
**Category:** decisions / output
**Priority:** Low

List output shows title, score, tag, description — but not provenance session. Need `--verbose` or always-show for session ID.

- [x] Show session ID in decisions list output

---

### 25. No decisions export/import

**Source:** Blood agent
**Category:** decisions / data portability
**Priority:** Low

Can't export registry to markdown or import from structured files. Users maintain parallel docs that drift out of sync.

- [ ] Consider decisions export --format md
- [ ] Consider decisions import

---

### 26. Thread can't filter by role

**Source:** Blood agent
**Category:** thread / filtering
**Priority:** Low

User continuation summaries dominate output. `--role assistant` would show only analysis/conclusions where decisions live.

- [x] Add `--role` filter to thread

---

### 27. conflicts --topic can't exclude sessions

**Source:** Blood agent
**Category:** conflicts / filtering
**Priority:** Low

When resolving conflicts, current session chain creates noise. `--exclude-session` would help.

- [x] Add `--exclude-session` to conflicts

---

### 28. No decisions supersede confirmation output

**Source:** Blood agent
**Category:** decisions / UX
**Priority:** Low

`decisions supersede` works silently. Should print what was superseded and by what.

- [x] Improve supersede output message

---

### 29. Wish: `detect --topic` to scope detection

**Source:** Blood agent
**Category:** detect / filtering
**Priority:** Low

Detect scans all text. `--topic` would scope to a pattern, reducing false positives.

- [x] Add `--topic` filter for detect
