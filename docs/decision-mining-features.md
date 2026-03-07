# Decision Mining Features

Features to support extracting, tracking, and threading design decisions from Claude Code conversation histories.

**Context:** A user with 452 sessions on a programming language project needs to retroactively catalog all design decisions and make future decisions easier to find. Snatch's search and analysis tools get ~70% of the way there. These features close the gap.

---

## Feature 1: Cross-Session Topic Threading (`snatch thread`)

**Problem:** Search finds isolated snippet matches. Decisions span multiple messages and recur across sessions. There's no way to say "show me everything about Drop trait across all sessions in chronological order with surrounding context."

**What it does:**
```
snatch thread -p blood "Drop trait" --chronological
```
- Searches all sessions for the pattern (like `search`)
- Groups matches by session
- Orders sessions chronologically
- For each match, includes the surrounding conversation context: the user message before, the matched message, and the assistant message after (or vice versa)
- Outputs a threaded narrative per topic, not isolated snippets

**Key design points:**
- Reuses existing search infrastructure (regex, fuzzy, scopes, filters)
- Results ordered chronologically across sessions by default — matches interleaved in the order they actually occurred, not grouped by session
- Context is message-level, not line-level — shows the full user prompt and assistant response around each match
- Deduplicates: if multiple matches occur in the same message exchange, collapse them
- Output formats: text (default), markdown, JSON
- Session headers with date and session ID for navigation

**Depends on:** Nothing new. Search + messages infrastructure already exists.

**Impact:** High for retroactive mining. Also useful for any future "show me the history of X" queries.

**Complexity:** Medium. Mostly composition of existing search and message retrieval, plus a new output formatter.

---

## Feature 2: Decision Registry (`manage_decisions`)

**Problem:** No structured store for "what did we decide about X?" Goals track intentions. Notes track tactical state. Nothing tracks decisions — the most important artifact of design conversations.

**What it does:**
```
snatch decisions add -p blood "No Drop trait - blood uses manual resource management" \
  --session abc123 --status confirmed
snatch decisions list -p blood
snatch decisions list -p blood --status confirmed
snatch decisions supersede -p blood 3 --by 7 --reason "Reversed after perf analysis"
```

**Schema:**
```json
{
  "id": 1,
  "title": "No Drop trait",
  "description": "Blood uses manual resource management, no implicit destructors",
  "status": "confirmed",
  "session_id": "abc123",
  "message_uuid": "optional - specific message where decided",
  "created_at": "2025-01-15T...",
  "updated_at": "2025-03-07T...",
  "confidence": 0.9,
  "superseded_by": null,
  "tags": ["memory-model", "traits"],
  "references": ["def456", "ghi789"]
}
```

**Status values:** `proposed`, `confirmed`, `superseded`, `abandoned`

**Storage:** `~/.claude/projects/<project>/memory/decisions.json` (same pattern as goals.json, notes.json)

**MCP integration:** `manage_decisions` tool exposed via MCP server, auto-injected by SessionStart hook so every session knows what's already been decided.

**Impact:** Highest long-term ROI. Once decisions are tracked, the retroactive mining problem disappears for future sessions. Also prevents contradictory decisions — the hook shows what's already confirmed.

**Complexity:** Medium. Follows the exact pattern of `manage_goals` and `manage_notes`. Storage, CLI, and MCP plumbing already exists as a template.

---

## Feature 3: Contradiction Detection (`snatch decisions conflicts`)

**Problem:** The motivating problem for this entire effort: inconsistencies and discrepancies between design decisions made across sessions. Features 1-2 help find and track decisions, but neither answers the core question: "where did we decide contradictory things?"

**What it does:**
```
snatch decisions conflicts -p blood
snatch decisions conflicts -p blood --topic "Drop"
```

**Detection approaches:**

1. **Registry-based (requires Feature 2):** Compare decisions in the registry that share tags or topics. Flag pairs where status changed (confirmed → superseded) or where descriptions contain opposing language about the same concept.

2. **Search-based (standalone):** For a given topic pattern, find all sessions where it was discussed, extract the conclusion from each (last assistant message in the exchange), and flag sessions where conclusions diverge. This is heuristic — it looks for opposing signal words ("will use X" vs "won't use X", "removed" vs "added", "yes" vs "no") applied to the same topic.

3. **Lessons-based:** Cross-reference user corrections from `lessons` — if the user corrected the same topic multiple times, the corrections themselves may be contradictory.

**Output:**
- Pairs of potentially conflicting decisions with session IDs, timestamps, and the relevant text
- Which is more recent (recency as tiebreaker)
- Confidence that they actually conflict (high: explicit reversal language; medium: different conclusions; low: topic overlap with different framing)

**Impact:** High. This is the actual problem that triggered the decision mining effort.

**Complexity:** High for standalone search-based detection. Medium if built on top of the registry (Feature 2) since structured comparison is easier than free-text contradiction detection.

**Depends on:** Works best with Feature 2 (registry), but the search-based approach can work independently.

---

## Feature 4: Decision Extraction Heuristic (`snatch decisions detect`)

**Problem:** Keyword search for decisions has poor precision. "Decided" appears in many non-decision contexts. Actual decisions follow a structural pattern in conversations that's detectable without AI.

**What it does:**
```
snatch decisions detect -p blood --no-limit
snatch decisions detect abc123
```

**Detection heuristic — structural pattern matching:**
1. User asks a question (interrogative prompt — contains `?`, or starts with question words)
2. Assistant responds with options/analysis (long response with enumeration: numbered lists, "option A/B", "approach 1/2", pros/cons patterns)
3. User confirms/chooses (short affirmative response — "yes", "let's go with", "option B", "I agree", etc.)
4. Optionally: assistant implements (tool calls follow in next turn)

**Also detects:**
- Explicit decision markers: "DEF-\d+", "design decision", "we decided", "the decision is"
- Reversal patterns: "actually", "changed my mind", "let's go back to", "scratch that"

**Output:**
- Candidate decision point with session ID, timestamp, and the question/answer exchange
- Confidence score based on pattern strength (explicit marker > structural pattern > weak signal)
- Can pipe into `manage_decisions` for confirmation and storage

**Impact:** High for retroactive mining. Automates the most labor-intensive part of the patterns-tsv sweep — interpreting whether a search hit is actually a decision.

**Complexity:** High. The structural pattern matching across message boundaries is new logic. The interrogative/affirmative classification needs tuning to avoid false positives.

---

## Feature 5: Message-Level Tagging

**Problem:** `tag` works on sessions, not messages. You can't mark a specific exchange as `#design-decision:drop-trait` for later retrieval. This means every decision requires retroactive search to find — there's no way to mark it at the moment it happens.

**What it does:**
```
snatch tag message <session_id> <message_uuid> "decision:drop-trait"
snatch search --tag "decision" -p blood
snatch search --tag "decision:drop-trait" -p blood
```

**Storage:** Sidecar file `~/.claude/projects/<project>/message-tags.json` (cannot modify original JSONL files).

**Integration points:**
- MCP tool `tag_message` — allows Claude to tag a message during conversation when a decision is made
- Search gains `--tag` filter to find tagged messages across sessions
- Export gains `--tagged-only` to export only tagged exchanges
- Coupling with Feature 4: detection heuristic identifies candidate moments, tagging confirms them

### Tag taxonomy

Users can create any freeform tag. But a conventional set of well-known tags enables auto-detection and consistent retrieval across projects.

**Auto-suggestible tags** (snatch can detect these from conversation structure and prompt for confirmation):

| Tag | What it marks | How detected |
|-----|---------------|--------------|
| `decision` | A design/architecture choice was made | Feature 4 heuristic: question → options → confirmation |
| `reversal` | A previous decision was changed | Reversal language patterns ("changed my mind", "scratch that") |
| `correction` | User corrected the AI | Already detected by `lessons --category corrections` |
| `bug` | Bug discovered or fixed | Already detected by `lessons --category errors` |
| `milestone` | Significant deliverable completed | Detectable from commit messages in tool calls, or explicit user language |

**Human-only tags** (require user judgment, can't be reliably auto-detected):

| Tag | What it marks | Why it can't be auto-detected |
|-----|---------------|-------------------------------|
| `insight` | Breakthrough understanding | Only the user knows what was a breakthrough vs routine |
| `trade-off` | Acknowledged compromise | Requires understanding what was given up |
| `open-question` | Unresolved, needs future attention | Requires knowing it wasn't resolved later |
| `revisit` | Explicitly marked for reconsideration | Intent-based, not pattern-based |
| `reference` | Important external resource | Distinguishing important links from incidental ones |

**Key design principle:** The value of tagging scales with automation. Freeform tags that nobody remembers to use are worthless. Auto-suggested tags that require a single confirmation have high adoption. Feature 4 (detection heuristic) and Feature 5 (tagging) are tightly coupled — detection identifies the moment, tagging persists the annotation.

**Impact:** Highest for prospective tracking. If decisions are tagged when made, no mining is needed.

**Complexity:** Medium. Sidecar storage is simple. The harder part is integrating tags into search, export, and the MCP layer. The auto-suggestion coupling with Feature 4 adds complexity but is where the real value is.

---

## Feature 6: Aggregated Search with Session Phase Context

**Problem:** A search match for "Drop trait" tells you it appeared in session abc123. But was that at the start of a fresh exploration, or 3 hours into debugging after a failed approach? The phase context changes interpretation.

**What it does:**
- Search results include session phase metadata:
  - Relative position (early / middle / late in session)
  - Time into session when the match occurred
  - Whether it was before or after a compaction
  - What the session was primarily about (from digest/first prompts)
- Enables filtering: `--phase early` (decisions made at session start are often more deliberate than mid-debugging tangents)

**Impact:** Medium. Adds interpretive value to search results but isn't blocking for the core mining task.

**Complexity:** Low-Medium. Session analytics already tracks timestamps and compaction events. It's mostly formatting.

---

## Feature 7: Decision Confidence Auto-Scoring

**Problem:** Not all decisions carry equal weight. Some were explicitly discussed, user-confirmed, and implemented. Others were drive-by assumptions never questioned. When cataloging decisions retroactively, confidence scoring helps prioritize which ones to verify.

**What it does:**

Auto-scores based on observable signals:
| Signal | Score Impact |
|--------|-------------|
| User explicitly confirmed (affirmative response) | +high |
| Implemented immediately (tool calls followed) | +medium |
| Discussed with options/tradeoffs | +medium |
| Appeared in thinking blocks only (never surfaced) | -high |
| User corrected later (found in lessons) | -high |
| Contradicted in a later session | -high |
| Repeated consistently across multiple sessions | +high |
| Never revisited or questioned | neutral |

**Depends on:** Decision registry (Feature 2) and detection heuristic (Feature 4) to have decisions to score.

**Impact:** Medium. Useful for triage but only after the registry exists.

**Complexity:** High. Cross-referencing decisions against corrections, contradictions, and implementation evidence across sessions is substantial analysis.

---

## Field Report: Real-World Mining of 452 Sessions

A parallel session used snatch as-is to mine design decisions from the blood project. Here's what actually happened.

### What worked well

- **`--patterns-tsv` single-pass sweep** — 42-pattern file across all sessions, extremely effective for heat-mapping which sessions contain decision activity
- **`search -c --with-date`** — per-session match counts for targeting deep dives
- **`prompts --all --grep "pattern" --exclude-system`** — finding user decision language
- **`digest`** — quick session context for orienting before deep-reading

### What was painful

1. **No `--sessions-only` filter** — `--files-only` (`-l`) lists matching session IDs but includes subagent sessions. Had to pipe through `grep -v "^agent-"` to exclude them. Should have a `--no-subagents` flag on search output (or `--sessions-only` that implies it).

2. **No `--aggregate-by-session`** — search lists every individual match. For heat-mapping, you want one line per session with total count. Currently requires `search -c` which gives a single total, not per-session breakdown. The `--with-date` flag helps but only in count mode.

3. **No chronological cross-session ordering** — results are grouped by session, not ordered by timestamp across sessions. When tracing the evolution of a topic, you want matches in the order they actually happened, interleaved across sessions. This is exactly Feature 1.

4. **Decision detection is entirely manual** — the sweep found where decisions *might* be, but every hit required manual interpretation. The agent had to read full message context to determine if a search match was actually a decision point. This is exactly Feature 4.

5. **Confidence scoring had to be done by hand** — the agent assigned confidence scores based on reading the conversations, noting things like "user confirmed Drop removal but never reviewed the replacement code" (high confidence on the decision, low confidence on the implementation). No tooling support for this.

### Key finding: formal ADRs already existed

The blood project had 37 formal ADRs in `docs/planning/DECISIONS.md` that the mining agent didn't initially know about. This highlights a gap: snatch mines *conversations* but has no awareness of *artifacts* produced by those conversations. The decision registry (Feature 2) would bridge this by being the canonical source, with ADR documents and session IDs both linked as references.

### Revised pain priority from real usage

Based on what the mining agent actually struggled with (not what we theorized):

1. **Cross-session topic threading (#1)** — most painful gap, would have saved the most time
2. **Search ergonomics (#8)** — small but frequent friction points
3. **Decision detection heuristic (#4)** — would have automated the most tedious part
4. **Decision registry (#2)** — would have prevented the problem entirely if it existed earlier

---

## Feature 8: Search Ergonomics (small improvements)

**Problem:** Several small friction points surfaced during real mining work. None justifies a major feature, but together they add up.

**Specific improvements:**

### 8a. `--no-subagents` filter on search output
Currently `--files-only` lists all matching session IDs including subagents. Add `--no-subagents` (or make it the default for `--files-only`) to exclude `agent-*` sessions from output.

### 8b. `--aggregate-by-session` output mode
One line per session with match count, ordered by count (descending) or date. More useful than `--count` (single total) for heat-mapping.
```
snatch search -p blood "Drop" --aggregate-by-session
# Output:
# 2025-02-14  abc123  12 matches
# 2025-01-20  def456   8 matches
# 2025-03-01  ghi789   3 matches
```

### 8c. `--patterns-tsv` per-session breakdown
Currently `--patterns-tsv` outputs aggregate counts per pattern. Add a mode that shows which sessions matched which patterns, not just totals. Essential for targeting deep dives after a sweep.
```
snatch search -p blood --patterns-tsv patterns.tsv --breakdown
# Output:
# decisions  explicit  DEF-markers  --thinking-only  DEF-\d+
#   2025-01-20  abc123  4
#   2025-02-14  def456  2
# decisions  implicit  user-confirms  -t user  agree|confirmed|go.with
#   2025-01-20  abc123  7
#   2025-03-01  ghi789  1
```

**Note:** Chronological cross-session ordering (previously considered as 7c) is folded into Feature 1 (Topic Threading), where it's a core design point rather than a search flag.

**Impact:** Medium individually, high collectively. These are the "papercuts" that slow down every search-heavy workflow.

**Complexity:** Low. All three are output formatting changes on existing search infrastructure.

---

## Triage Matrix

| # | Feature | Retroactive | Prospective | Complexity | Dependencies | Field-Validated Pain |
|---|---------|-------------|-------------|------------|--------------|----------------------|
| 1 | Topic Threading | **High** | Medium | Medium | None | **Highest** — most time lost |
| 8 | Search Ergonomics | **High** | Medium | **Low** | None | **High** — constant friction |
| 3 | Contradiction Detection | **High** | **High** | High (standalone) / Med (w/ #2) | Best with #2 | **High** — the motivating problem |
| 4 | Detection Heuristic | **High** | Medium | High | None (feeds #2) | High — most tedious manual work |
| 2 | Decision Registry | Medium | **High** | Medium | None | Medium — prevents future pain |
| 5 | Message Tagging | Low | **High** | Medium | None | Not tested |
| 6 | Phase Context | Medium | Low | Low | None | Low |
| 7 | Confidence Scoring | Medium | Medium | High | #2, #4 | Medium — done manually |

### Dependency graph

```
Feature 8 (Search Ergonomics) ── no deps, quick wins
Feature 1 (Topic Threading) ── no deps
Feature 2 (Decision Registry) ── no deps
Feature 4 (Detection Heuristic) ── standalone, but output feeds into #2 and #5
Feature 5 (Message Tagging) ── standalone for manual tags; auto-suggestion requires #4
Feature 3 (Contradiction Detection) ── standalone possible, much better with #2
Feature 6 (Phase Context) ── no deps
Feature 7 (Confidence Scoring) ── requires #2 and #4
```

### Recommended build order

**Phase 1 — Quick wins + foundation:**
- **8** (Search Ergonomics) — low effort, immediate payoff for all search workflows
- **2** (Decision Registry) — medium effort, unlocks #3 and #7, forward-looking foundation

**Phase 2 — Retroactive power tools:**
- **1** (Topic Threading) — the biggest structural gap for mining existing sessions
- **4** (Detection Heuristic) — automates the tedious interpretation step

**Phase 3 — Advanced analysis:**
- **3** (Contradiction Detection) — the motivating problem, benefits from registry data
- **5** (Message Tagging) — prospective tracking for future sessions

**Phase 4 — Polish:**
- **6** (Phase Context) — interpretive enrichment
- **7** (Confidence Scoring) — automated quality assessment
