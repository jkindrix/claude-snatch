# Multi-Provider Ingestion Design

**Status:** Architecture confirmed (decision #30, 2026-07-17); contracts amended
same day after external review by a Codex agent (gpt-5.6-sol) running inside the
target tool — review verified point-by-point before adoption (see "Review
amendments" below).
**Goal:** goal #18 — extend snatch (CLI + MCP) to ingest and analyze session logs
from agentic coding tools beyond Claude Code, designed for future extensibility.
First target: OpenAI Codex CLI.

## Architecture decision

**Option C — provider seam + normalize into the existing internal model.**

1. Extract a `SourceProvider` seam at the pipeline's ends (discovery +
   parse-to-entries). Claude Code becomes provider #1 with zero behavior
   change; that refactor is its own verifiable milestone. This is the missing
   dual of the existing `Exporter` seam.
2. Normalize other providers into `LogEntry` (the crate's universal currency:
   355 match sites across 41 files) under the fidelity and identity contracts
   below.
3. Provider semantics are pushed into normalization as canonical annotations
   (see "Semantic annotations"), not as provider conditionals scattered
   through the middle layers.

Rejected: **A (pure adapter)** — fastest but silently bleeds CC semantics and
weakens the fidelity story; **B (trait-generic middle)** — rewrites the whole
middle (41 files) for isolation the evidence says normalization already
provides.

## Fidelity contract

Raw fidelity lives at the **session source**, not inside normalized entries.
Native lines are NOT embedded in `LogEntry` (that would duplicate sensitive
payloads through clones/caches, break on 1:N and N:1 record↔entry mappings,
undermine redaction — sanitized canonical content must not carry an
unsanitized native copy — and has no meaning for compressed or DB-backed
sources).

Instead, a parsed session carries provenance whose cardinality is explicit —
one native record may produce several entries, several records may collapse
into one entry, and some records produce none:

```rust
struct ParsedSession {
    descriptor: SessionDescriptor,
    entries: Vec<LogEntry>,
    entry_origins: Map<EntryId, Vec<RecordRef>>,   // N:1 and 1:N expressible
    record_dispositions: Vec<RecordDisposition>,   // self-identifying:
    diagnostics: IngestionDiagnostics,
}
// A disposition names its record — a bare list cannot identify records
// across multiple artifacts without relying on implicit ordering.
struct RecordDisposition { record: RecordRef, outcome: RecordOutcome }
enum RecordOutcome {
    Mapped(Vec<EntryId>),
    Suppressed { reason: SuppressionReason },
    Unknown,
    Unparseable { error: ParseDiagnostic },
}
// entry_origins is the reverse index and is validated for consistency
// against the outcomes.
```

`record_dispositions` is what makes acceptance invariant #1 enforceable: every
native record has exactly one disposition. `RecordRef` is artifact identity +
record ordinal — no content hashes (unnecessary absent a corruption-detection
requirement, and hashes of low-entropy sensitive text leak equality
information). The provider exposes separate archive/native/raw operations that stream to a
caller-supplied writer — archives and compressed logs can be multi-gigabyte
and must never require full in-memory buffering. The archive tier's
lossless promise is testable: native records must round-trip out of the
bundle.

Export promises are **capability-tiered per provider** (an extension of
decision #24's archival/complete/readable contract):

- **archive (universal tier)**: a lossless, provider-defined bundle with a
  manifest. File providers deliver exact artifact bytes; DB providers deliver
  lossless value/schema preservation of the session's records (a DB row has no
  independent byte representation, and copying a whole database would bundle
  unrelated sessions).
- **native (optional, stronger)**: exact source-artifact bytes, including
  `.jsonl.zst`. Advertised only where a discrete source artifact exists.
- **raw-jsonl (optional)**: the provider's unmodified JSONL record stream
  (decompressed where applicable). Only JSONL-backed providers advertise it.
- **json/jsonl (normalized)**: snatch's representation, content-complete.
  Normalized output carries **machine-readable provider and derivation
  metadata** — documentation alone will not stop consumers from mistaking
  synthesized Claude-shaped fields for native data.

Synthesized linkage (e.g. an ordering parent edge) is documented as **derived
ordering, never native causality**.

## Identity and provider context

- **Logical identity and artifact identity are separate.** One logical
  session can have several physical artifacts: active + archived copies,
  plain + `.zst` twins, backup/imported copies under multiple roots (the
  cc-archive setup on this machine is a live example), and forks containing
  copied source history.

  ```rust
  // namespace: provider-defined; equivalent backup roots share one,
  // genuinely separate installations cannot collide accidentally
  // (matters for providers with database-local integer ids).
  LogicalSessionKey { provider: ProviderId, namespace: SessionNamespace, native_id: String }
  // revision is NOT part of identity — an append to an active session
  // must not mint a new artifact identity.
  ArtifactId { provider_instance, locator }
  ArtifactRevision(/* opaque provider token */)
  ArtifactSnapshot { id: ArtifactId, revision: ArtifactRevision }
  SessionDescriptor { key, artifacts, preferred_artifact, ... }
  ```

  Twin precedence (pinned in A.0): active over archived, plain/database over
  compressed, final tie-breaker = stable ArtifactId ordering (never
  discovery order, which filesystems do not guarantee between runs).
  Descriptors validate non-empty, id-unique artifact sets. Native ids alone are not unique across providers, and "path to a
  JSONL file" is not a valid universal identity once DB-backed providers
  exist.
- Provider context flows past `Session` into parsed sessions, `Conversation`,
  analytics, and exports — `Conversation::from_entries` currently takes only
  `Vec<LogEntry>` (src/reconstruction/mod.rs), which is exactly where provider
  information would otherwise die.
- The parse cache (currently keyed by path+mtime, src/cache/mod.rs) is re-keyed
  by `LogicalSessionKey` + a provider-supplied revision token (path/size/mtime for
  files; row/index revision for databases).
- Normalized entry ids are deterministic, unique, and **structured**
  (`EntryId { session: LogicalSessionKey, ordinal, subindex }`) — identity
  never depends on string encoding. The external encoding escapes segments
  (`%`→`%25`, `:`→`%3A`) and always includes the namespace, so hostile
  delimiters cannot make distinct keys render identically; the qualified
  display form of session keys escapes the same way. Every entry id must
  belong to its session's descriptor key (validated). `turn_id` is retained as
  its own canonical field — it is NOT mapped onto `message.id`, which snatch
  uses to group streaming chunks and count assistant messages (overloading it
  would silently redefine "assistant message" as "turn").

## Semantic annotations (adapter output, middle stays neutral)

The adapter emits canonical annotations; middle-layer logic keys on these, not
on provider. **Carrier decision (Phase A.0):** annotations live as fields on
the relevant existing semantic types (prompt metadata on user messages,
canonical kind on tool calls, usage scope on usage observations) plus
session-level context on `ParsedSession` — NOT a universal
`CanonicalEntry { entry, semantics }` wrapper, which would recreate Option B's
blast radius. A sidecar keyed by deterministic entry id is the fallback where
a field placement doesn't exist.

The enums below are illustrative, not frozen:

- Prompt semantics are **two axes**, authorship and delivery mode (a steered
  message is human-authored but mid-turn-delivered) — feeds `is_human_prompt`
  / prompt-boundary chunking.
- `ToolKind` + preserved native tool name — feeds tool analyses/lessons.
- Usage semantics need **scope and aggregation as separate dimensions**:
  Codex `token_count` events carry both last-call usage and
  session-cumulative usage side by side.
- `LineageEdge { from, to, kind: LineageEdgeKind::{Continuation, Fork,
  Spawn} }` — session lineage is a typed graph with a real carrier:
  `SourceProvider::lineage()` returns the corpus's edges (CC resume chains
  are Continuation, Codex forks Fork, subagents Spawn). Compaction window
  links are intra-session metadata, not lineage edges.
- Tool semantics are **per tool call** (keyed by native call id — one entry
  can carry several calls with different classifications); usage
  observations carry **their own values** alongside scope+aggregation so
  annotations are never separated from the numbers they describe.
- `CompactionKind` exists; the carrier for compaction window metadata is
  explicitly deferred to Phase B3/C.

Provider identity remains available for reporting and exceptional cases, but
is not the primary semantic switch. Two gates stay table-driven rather than
annotation-driven: model pricing (a rates table; see Pricing) and
content-shaped noise filters in lessons (tuned per provider in Phase C).

## Pricing

Codex sessions are **unpriced by default**. Official Codex pricing
distinguishes ChatGPT plan/credit usage from API-key token billing; applying
API per-token rates to a ChatGPT-authenticated session would fabricate a cost
the user never paid. If pricing is added later: label it "API-equivalent
cost", require an explicit pricing mode, and treat it as actual estimated
spend only when the session itself reliably records an API-billed
provider/auth mode. Do not infer historical billing from current auth.json.

## Evidence base (research round, 2026-07-17)

### Codex rollout format (corpus of 222 real files, Sep 2025–Jul 2026, + source at rust-v0.144.5)

- Envelope: one JSON object per line, `{timestamp, type, payload}`
  (`RolloutLine`/`RolloutItem`, `codex-rs/protocol/src/protocol.rs`). Types:
  `session_meta`, `response_item`, `event_msg`, `turn_context`, `compacted`,
  `world_state` (new 2026-07), `inter_agent_communication(_metadata)`.
- Two parallel streams: `response_item` = model-API record (message with
  user/assistant/developer/system roles, `reasoning`, `function_call`/
  `function_call_output` joined by `call_id`, `custom_tool_call`
  (apply_patch), `local_shell_call`, `web_search_call`); `event_msg` =
  UI/runtime record (`user_message`, `agent_message`, `agent_reasoning`,
  `token_count`, `turn_started/complete/aborted`, `thread_rolled_back`,
  `context_compacted`, review-mode events). Content duplicated across both;
  dedup policy needed (lean: response_item authoritative for content,
  event_msg for user-facing text and token counts — validate empirically in
  Phase B3).
- No per-line ids, no parent links, no version field. Ordering is flat append;
  `turn_id` groups turns (also on `turn_context` and via
  `internal_chat_message_metadata_passthrough`); `session_meta.cli_version`
  is the only version marker.
- Resume **appends to the same file**. Fork creates a new file (new UUIDv7
  thread id, `forked_from_id` in meta) and copies truncated history in —
  including the source's `session_meta` lines (16 such files in the corpus).
- Subagents: separate rollout files with `parent_thread_id` +
  `source: {subagent: …}`; `thread_spawn_edges` table in state DB.
- Compaction: `compacted` items carry summary + full `replacement_history` +
  UUIDv7 window-id chain (`window_id`/`previous_window_id`/`first_window_id`/
  `window_number`) — richer than CC's compact boundary. Note:
  `replacement_history` must not be counted as new chronological activity.
- Reasoning: Codex MAY persist plaintext reasoning summaries when they are
  emitted; availability varies by era/model/configuration. Measured on this
  corpus: Nov 2025–Mar 2026 sessions have summaries on 85–99% of reasoning
  items plus `agent_reasoning` events; from Apr 2026 onward both are 0% —
  encrypted payloads only, mirroring modern Claude Code. `encrypted_content`
  is an opaque encrypted reasoning payload (no guarantee it is "full CoT").
  Doctor must report summary availability / empty rates per corpus.
- Metadata: `session_meta` has session/thread id (UUIDv7 = filename uuid),
  cwd, originator, cli_version, source (cli/vscode/exec/mcp/subagent…),
  `git {commit_hash, branch, repository_url}`, model_provider,
  base_instructions, history_mode. `turn_context` per turn: cwd, model,
  approval/sandbox policy, collaboration_mode, effort.
- Layout: `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<local-ts>-<uuid>.jsonl`,
  archived twin under `archived_sessions/`, cold files as `.jsonl.zst` (since
  ~v0.137, early June 2026; plain `.jsonl` wins when both exist),
  `session_index.jsonl` (thread names), `state_5.sqlite` (threads index —
  rebuildable from JSONL; 40 sqlx migrations), `history.jsonl` (prompt-only).
- Known format hazards:
  1. Files from Codex ≤0.31.0 (before 2025-09-10) use a bare pre-envelope
     format (naked ResponseItem lines, minimal meta, `record_type: state`
     markers). This corpus is entirely envelope-era; others won't be.
  2. `history_mode: "paginated"` (per-thread, default legacy) changes the
     persisted vocabulary to `item_completed` TurnItems; a merged post-0.144.5
     PR adds ordinals. Schema is actively moving.
  3. session_meta field sets drift heavily across 10 months (base_instructions
     82/238, thread_source 77/238, history_mode/context_window 19/238);
     2 unparseable lines exist in the corpus. Doctor-style drift detection is
     mandatory from day one.

### snatch coupling map (chokepoints)

Pipeline: discover → parse → `LogEntry` → `Conversation` → analyses/export.
Coupling concentrated at the ends. Chokepoints: `LogEntry`
(model/message.rs), `JsonlParser` (parser/mod.rs — salvage anchors are
CC-shaped), `Conversation::from_entries` (reconstruction — needs
uuid/parentUuid/message.id), `Session`+`ClaudeDirectory` (discovery — layout,
lossy path decode, sidecars, chains), `is_human_prompt`/`is_prompt_boundary`,
`ModelPricing::for_model`, `SessionAnalytics::from_conversation`, `Exporter`
(the one existing seam, provider-neutral already), `ContentBlock`+tool_names.
`LogEntry` is already format-tolerant (Unknown variants, WithUnknown flatten,
Other(String) enums) — the fidelity-hardening work is what makes Option C
cheap.

### Tool landscape (July 2026)

Format families: (a) CC-style per-project JSONL with uuid/parentUuid trees —
Qwen Code (near-clone of CC incl. path sanitization), Pi (only tool with an
officially documented, versioned format), Gemini CLI post-2026-04 (JSONL),
Copilot CLI `events.jsonl` + SQLite index, Mistral Vibe, Kimi; (b) SQLite with
JSON payload columns — opencode ≥1.2, Goose ≥1.10, Cursor (closed,
part-protobuf), Crush, Warp, Devin; (c) single JSON per session — Amp (uses
Anthropic-style content blocks), Cline, old Gemini; (d) markdown — Aider.
Four tools migrated storage formats mid-life within ten months; almost nothing
is versioned in-band. No interchange standard fit for internal use: ATIF
(Harbor, v1.7) is the closest semantic match but young/lossy on DAG lineage,
compaction, queued-prompt provenance; OTel GenAI semconv is span-shaped and
experimental. Both are candidate *export* targets later. Conclusion: own
internal model, native logs as source of truth, adapters per provider — and
the `SourceProvider` trait must support DB-backed discovery, not just
directory walks.

## Phase plan

- **Phase A.0 — type-contract pass (opens Phase A, before touching call
  sites).** First deliverable is deliberately narrow: (a) the identity,
  artifact, revision, provenance, capability, and semantic metadata types;
  (b) unit tests exercising them through the deliberately awkward fake
  provider; (c) a review checkpoint before the types are threaded through
  existing call sites. Pins: (1) archive/native capability semantics; (2)
  provenance cardinality and record accounting (self-identifying
  `record_dispositions` + `entry_origins` consistency); (3) where semantic
  annotations live (carrier decision above); (4) logical sessions vs physical
  artifacts (identity separate from revision, namespaces, twin precedence,
  duplicate detection, export artifact selection).
- **Phase A — seam + identity (refactor, zero behavior change).**
  `SourceProvider` trait, `LogicalSessionKey`/`ArtifactKey`/
  `SessionDescriptor`, provider capabilities, parsed-session context
  threading, archive/raw-source delegation, cache re-keying
  (`LogicalSessionKey` + provider revision token). `ClaudeCodeProvider` is
  the production impl; a fake in-memory provider exists from day one and
  deliberately stresses the seam — **non-file-backed, no raw-jsonl tier, one
  session with multiple artifacts** (a fake that merely resembles Claude
  JSONL would not test the seam honestly). Provider-selection UX is
  *designed* here (flags, qualified ids). Acceptance: full suite + snapshot
  exports byte-identical; Claude CLI/MCP/library behavior unchanged.
  Round-6 guardrails for this phase: the fake's multi-tool entry becomes a
  real assistant entry with two ToolUse blocks, and semantic call ids are
  validated against actual calls; lineage tolerates dangling endpoints
  (real corpora reference deleted/unavailable parents — keep the edge).
- **Phase B1 — Codex inventory & decoding.** Discovery of plain, archived,
  compressed (`.zst`, with decompressed-size limits), active/truncated, and
  legacy (pre-envelope) files; envelope parser; native diagnostics. Legacy
  files: recognized, inventoried, diagnosable, native/raw-exportable;
  normalized analysis reports `unsupported-legacy` until real provenance-
  documented fixtures justify a parser. Defer `state_5.sqlite` acceleration
  until profiling proves need.
- **Phase B2 — provider UX.** `--provider claude-code|codex|all` (repeatable),
  qualified ids (`codex:<uuid>`; unqualified prefixes allowed when unique;
  round-6 guardrail: FromStr/decoding + round-trip tests for the escaped
  qualified-id encoding BEFORE ids become CLI/MCP inputs),
  `snatch providers` (roots, session counts, format families, diagnostics),
  MCP requests gain optional `provider`, responses always carry provider +
  qualified session id. Default remains Claude-only until Phase D.
  Milestone: list/info + native/raw export work on real Codex sessions.
- **Phase B3 — normalization.** Round-6 guardrail: a turn_id carrier must
  exist before normalization (the design promises its preservation).
  Deterministic entry ids, two-stream
  reconciliation under invariant #3's emission-identity rule, typed fork/spawn
  lineage, steered-prompt and `world_state`/`ghost_snapshot` semantics settled
  empirically. Milestone: messages/timeline/normalized exports.
- **Phase C — semantic tuning.** Codex prompt-boundary chunking, lessons
  noise filters, usage semantics (scope + aggregation), reasoning-availability
  reporting in doctor, compaction-window presentation.
- **Phase D — cross-provider UX.** Unified project identity across providers
  (cwd/git), union views, default-provider switch consideration, and registry
  storage: goals/notes/decisions currently live under Claude-owned project
  storage (`~/.claude/projects/<enc>/memory/`) — either scope those MCP
  operations per-provider or migrate storage before claiming unified
  projects. Candidate export targets: ATIF, OTel GenAI.

## Acceptance invariants (before Codex is "supported")

1. Every native record is mapped, intentionally suppressed with a recorded
   reason, classified unknown, or reported unparseable — never silently
   dropped.
2. Normalized ids are stable across repeated parsing and append-only growth.
3. Two-stream reconciliation: records representing the same semantic emission
   render once; distinct emissions remain distinct even when their text is
   identical (dedup keys on emission identity, never on text equality); token
   usage is never double-counted.
4. Compaction `replacement_history` is not counted as new chronological
   activity; fork-copied history is retained when viewing the fork
   independently but not double-counted as new work in cross-session views.
5. Plain and compressed versions of a session normalize identically;
   decompressed-size limits prevent compression bombs.
6. Active files with partial final lines do not generate permanent drift
   findings; drift diagnostics record unknown *nested field paths*, not only
   unknown top-level record types.
7. Normalized output carries machine-readable provider + derivation metadata.
8. Compatibility is phased, so #7 does not contradict it: during Phase A,
   existing Claude outputs and snapshots are byte-identical; from B2 onward,
   existing inputs and semantics stay backward-compatible while additive
   provider/derivation metadata is permitted (an explicitly versioned
   normalized envelope if additive fields ever become unsafe); Claude
   raw-jsonl remains byte-identical permanently. Tests encode these three
   promises separately.

## Review amendments (2026-07-17)

An external review by a Codex agent (gpt-5.6-sol, xhigh) running in this repo
was verified and adopted:

- Raw fidelity moved out of `LogEntry` to source-delegated streaming +
  `EntryOrigin` provenance (verified: raw exporter already bypasses parsing,
  src/cli/commands/export.rs; redaction/memory/N:1 hazards real).
- `SessionKey` qualified identity + provider context through `Conversation`
  (refined to `LogicalSessionKey`/`ArtifactKey` in round 2)
  and the cache (verified: cache keyed path+mtime, src/cache/mod.rs;
  `from_entries` takes bare entries).
- Rejected `message.id := turn_id`; deterministic entry ids instead.
- Provider-parameterized gates reframed as adapter-emitted semantic
  annotations.
- "Chain" replaced by typed lineage graph (Continuation/Fork/Spawn).
- Reasoning claim corrected after re-measurement (summaries 85–99% in
  Nov 2025–Mar 2026 era, 0% from Apr 2026 — the corpus aggregate had masked
  the collapse).
- Codex unpriced by default (ChatGPT-plan vs API billing distinction).
- Phase B split into B1/B2/B3; provider UX pulled forward from D to A/B2;
  fake provider added to A; registry-storage scope surfaced in D; acceptance
  invariants adopted.

## Review round 2 (2026-07-17, same Codex agent)

Verdict: no remaining architecture/phase-ordering objections; Phase A gated on
the A.0 type-contract pass. Adopted: archive-vs-native tier split (a DB row
has no independent byte representation); explicit provenance cardinality
(`entry_origins` map + `record_dispositions`) with ordinal-only RecordRefs (no
content hashes — equality leakage on sensitive low-entropy text); annotation
carrier decision (fields on semantic types, not a universal wrapper) +
authorship/delivery and scope/aggregation axis corrections; logical-vs-
artifact identity split with twin precedence; honest fake-provider
requirements; invariant #3 reworded to semantic-emission identity (text-
equality dedup would merge legitimate repeats); machine-readable derivation
metadata, nested-path drift, fork-history double-count guards. The round also
verified the two previously unverified doc claims (Codex plan-vs-API billing
distinction; resume/fork/archive documented as stable operations).

## Review round 3 (2026-07-17, same Codex agent)

Verdict: proceed with Phase A.0; stop cycling the design doc. Adopted into
A.0: identity separated from revision (ArtifactId/ArtifactRevision/
ArtifactSnapshot); provider-defined SessionNamespace in LogicalSessionKey;
invariants #7/#8 reconciled as a phased compatibility contract;
RecordDisposition made self-identifying; stale UsageBasis / "no duplicated
text" shorthand cleaned up.

## Review round 4 (2026-07-17, same Codex agent — A.0 checkpoint)

Checkpoint review found six contract holes in the first A.0 cut; all fixed in
place: (1) EntryId now derives from the full LogicalSessionKey (namespace
included) — the fake's cross-namespace sessions previously produced colliding
ids; (2) entries carry their ids (IdentifiedEntry) and validate_provenance
cross-checks entries/origins/dispositions/semantics/diagnostics — the earlier
validator compared the maps only against each other; (3) Unknown outcomes now
carry preserved entries (content-complete under drift) and fork-inherited
history is Mapped + ActivityKind::InheritedHistory rather than suppressed;
(4) fidelity tiers stream to caller-supplied writers and the fake's archive
is a real manifest+records bundle with a round-trip test (the previous fake
archive was a hollow string checked with is_ok()); (5) real semantic
carriers exist (EntrySemantics sidecar: PromptSemantics, ToolSemantics with
ToolKind + native name, UsageObservation) — emitted by the fake, consumed by
tests; (6) twin-precedence tie-breaking uses stable ArtifactId ordering with
reordering-stability and descriptor-validation tests.

## Review round 5 (2026-07-17, same Codex agent — A.0 re-review)

Three blockers, all fixed in a scoped amendment with adversarial tests:
(1) qualified ids were still not injective — colon concatenation of
unrestricted segments let namespace "a" + native "b:c" collide with
namespace "a:b" + native "c", and the global display form was ambiguous the
same way; EntryId is now a structured type and both external encodings
escape segments reversibly; the validator also enforces that every entry id
belongs to the session's descriptor key. (2) Semantic cardinality: tool
semantics are per-call (BTreeMap keyed by native call id) and usage
observations embed their own Usage values. (3) The lineage graph gained its
carrier: LineageEdge { from, to, kind } + SourceProvider::lineage(). Cleanup:
the validator rejects duplicate entry ids within a producing outcome and
duplicate record refs within an origin list. CompactionKind window-metadata
carrier explicitly deferred to B3/C. 18 contract tests (was 15).

## Review round 6 (2026-07-17, same Codex agent — A.0 sign-off)

No remaining architectural or type blockers; Phase A greenlit with
byte-identical Claude behavior as the gate. Four non-blocking guardrails
folded into the phase checklists above: real multi-ToolUse fake entry +
call-id validation (A), dangling lineage endpoints allowed (A), qualified-id
FromStr/round-trip before CLI/MCP input use (B2), turn_id carrier before
normalization (B3).

## Review round 7 (2026-07-17, same Codex agent — milestone 1.5)

Four adapter issues fixed while still additive: (1) subagent logical
identity is parent-qualified (namespace `subagent:<parent>[:<workflow-dir>]`
— agent ids are only unique per parent), and duplicate native ids across
roots/projects merge into one descriptor with multiple artifacts; (2) the
contract gained `RecordOutcome::Recovered { entries, error }` (+ a
`recovered` diagnostics counter) and the adapter reuses the established
parser's torn-line salvage — record status is now separable from produced
entries; a provider-level immutable `max_file_size` mirrors the CLI option
until limits are threaded; (3) `write_native` resolves artifact ids against
discovered artifacts and streams the stored path — the previous lexical
`starts_with` check was forgeable via `<root>/../` traversal (tested); (4)
continuation edges derive from direct parent links independent of
complete-chain reconstruction (dangling parents keep their edges), Spawn
edges carry sidecar metadata (tool_use_id/agent_type/description) as
LineageEdgeKind::Spawn fields, and lineage output is sorted + deduplicated.
27 provider tests. Known inherited limitation noted in the adapter: Claude
discovery dedupes identical agent ids within one project (most-recent wins)
before the provider sees them.

## Review round 8 (2026-07-17, same Codex agent — milestone 1.5 re-review)

Three adapter-hardening fixes before threading: (1) the archive is a framed
multipart bundle — the manifest carries per-artifact byte lengths and the
body concatenates EVERY artifact (divergent duplicate copies preserved;
previously only the preferred copy was archived while the manifest claimed
both; the fixture's copies now genuinely diverge so the test bites);
(2) same-project subagent id collisions are content-complete: the provider
enumerates each parent's subagent_links() and merges by parent-qualified
key, recovering transcripts discovery's per-project id-dedup hides
(tested with the same agent id under two parents in ONE project);
subagent_namespace uses the LAST `subagents` path component (an ancestor
dir may share the name); (3) lenient-parser parity on read errors: an
invalid-UTF-8 line becomes an Unparseable disposition and parsing
continues (previously it aborted the whole session — a strict regression
vs the established parser; characterization test: valid → invalid UTF-8 →
valid), and metadata errors in the max-size check propagate instead of
reading as zero. 28 provider tests.

## Open questions (to settle in-phase)

1. Two-stream dedup policy details (B3, empirical — under the emission-
   identity rule of invariant #3).
2. Steered/queued prompt persisted shape (B3, empirical — inferred from
   inject.rs code paths only).
3. `world_state` / `ghost_snapshot` semantics (B3).
4. Twin precedence + duplicate detection rules across roots (A.0).
5. Annotation carrier field placements per semantic type (A.0).
6. Default-provider switch to `all` (D, with union view).
