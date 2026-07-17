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
- `CompactionKind` and `CompactionWindow` carry the typed boundary and window
  metadata; provider-neutral presentation remains Phase C work.

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
  STATUS (2026-07-17, round 10): **Phase A foundation complete** — the
  honest framing. Delivered: seam + contracts + hardened ClaudeCodeProvider;
  logical_key(&Session) shared between adapter and pipeline; Conversation
  carries an inert source (from_entries_with_source), threaded at the MCP
  shared-resolution funnel; the cache is keyed by typed
  CacheIdentity::{File(PathBuf), Provider(LogicalSessionKey)} with opaque
  revision comparison, exercised by the fake provider (File identity stays
  a lossless PathBuf — a display-string rendering collides on non-UTF-8
  paths, regression-tested). NOT yet delivered (explicitly moved to B2/B3):
  production routing through SourceProvider (MCP paths still call
  Session::parse directly; the library API builds source-less
  conversations; archive/raw delegation has no production caller),
  parsed-session propagation, and export delegation. The construction-site
  deferral covers CLI, MCP, and library/API sites alike; when provider-aware
  consumers arrive, a centralized Conversation::from_parsed_session(...)
  path is preferred so provenance, semantics, and source cannot be
  independently forgotten.
- **Phase B1 — Codex inventory & decoding.** Opening guardrails (round 11):
  the provider cache token must cover the selected ArtifactId +
  ArtifactRevision (or an aggregate provider snapshot token over every
  artifact affecting parsing) BEFORE B1's first production cache consumer —
  a changed preferred artifact with a coincidentally identical revision
  string must not hit stale content (test required). zstd 0.13.3
  (default-features off) behind `codex = ["dep:zstd"]` gating the WHOLE
  provider — no configuration may silently ignore compressed sessions;
  codex enabled by default at release. Decode through a streaming reader
  with compressed-input and decompressed-output limits plus window_log_max
  (never decode_all); test limit crossing, truncation, checksum failure,
  and plain/.zst normalization equivalence. Fixture policy: synthetic and
  PII-free with a manifest (Codex version/format family, sanitization or
  synthesis method, expected diagnostics); at least one sanitized .zst
  generated EXTERNALLY (interop must not be tested solely against this
  library's own encoder); fixtures for active truncation, unknown nested
  fields, duplicate streams, legacy detection, and decompression limits; a
  separate opt-in real-corpus conformance test emitting aggregate results
  only, never in public CI. Discovery of plain, archived,
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

## Review round 10 (2026-07-17, same Codex agent — Phase A close review)

Verdict: B1 may proceed; Phase A is renamed "foundation complete" rather
than closed as fully threaded. Fixes adopted: (1) T2's string-keyed cache
regressed non-UTF-8 path identity (distinct paths render identically via
replacement characters) and delivered no provider-keyed API — replaced with
typed CacheIdentity::{File(PathBuf), Provider(LogicalSessionKey)} +
Revision::{FileMtime, Opaque}, get_keyed/insert_keyed validated against
caller-supplied tokens, exercised by the fake provider, with a unix
regression test proving the display-string aliasing; file persistence
format unchanged (provider entries are rebuilt, not persisted).
(2) Production routing, parsed-session propagation, and export delegation
explicitly moved to B2/B3 (recorded above). (3) The construction-site
deferral inventory now includes MCP and library/API sites, with a
centralized from_parsed_session(...) path preferred when consumers arrive.
18 cache tests (was 16), 28 provider tests.

## Review round 11 (2026-07-17, same Codex agent — B1 greenlight)

Phase B1 greenlit; no further architecture cycle. Adopted as B1 opening
guardrails (recorded in the Phase B1 section): aggregate cache revision
token before the first production cache consumer; zstd 0.13.3
(default-features off, verified on crates.io: 330M downloads) behind a
whole-provider `codex` feature, default-on at release; streaming decode
with input/output limits and window_log_max; the expanded fixture policy
(manifest, external .zst, truncation/drift/duplicate/legacy/limit
fixtures, opt-in aggregate-only real-corpus conformance). Next checkpoint:
after Codex discovery, decoding, diagnostics, and sanitized fixtures exist.

## Phase B1a shipped (2026-07-17)

CodexProvider (feature `codex`, zstd 0.13.3 default-features-off):
discovery across sessions/ + archived_sessions/ with plain/.zst twin
merging and filename thread-id parsing; envelope-vs-legacy format sniffing;
B1-honest parse (envelope records preserved as LogEntry::Unknown with
Unknown dispositions — normalization is B3); legacy files inventoried and
native/raw-exportable but refused for normalization; streaming zstd decode
with window_log_max=31 and a decompressed-output cap (limit-crossing,
truncation, twin-equivalence tested); framed multipart archive; fork/spawn
lineage from first-meta fields; opt-in aggregate-only real-corpus
conformance test (never in CI). Real-corpus run: 224/224 sessions parsed,
0 errors, 0 provenance violations, 222,108 records preserved, 2 unparseable
lines (matching the original census). Observation for B3: this corpus's
fork files carry no forked_from_id in their first meta (field postdates
them) — fork lineage needs the embedded-second-meta heuristic.

## Review round 12 (2026-07-17, same Codex agent — B1a hardening)

Five gaps folded into B1b, each with a test built to fail pre-fix:
(1) bounded decompression completed — compressed-input cap added,
LimitedReader EOF-probes so a stream exactly at the limit is valid
(exact/one-short tested), window_log_max lowered from the 2 GiB format
ceiling to zstd's practical 128 MiB refusal default (2^27), corrupt-frame
streams surface as dispositions; (2) byte-level record reading
(read_until + from_slice) so invalid UTF-8 mid-stream no longer loses
later records, plain and compressed both tested; (3) inventory preserves
authoritative PathBufs (locator strings are display-only — non-UTF-8
CODEX_HOME round-trips through parse/native/archive, tested) and the walk
has a deliberate no-follow symlink policy (cycle + external-file symlink
tested); (4) sniffing is envelope-SHAPE based (string timestamp + string
type + payload), forward-compatible with unknown first-record types
(tested), with an explicit Undetermined policy documented; (5) artifacts
sort by stable identity (determinism test) and the corpus conformance test
compares disposition totals against an independent per-artifact record
count and emits aggregate-only output. Re-run on the real corpus: 224/224
parsed, 0 errors, 0 violations, 0 count mismatches, 222,192 records,
2 unparseable. 17 codex tests + 1 opt-in conformance.

## Review round 13 (2026-07-17, same Codex agent — round-12 follow-up)

Four corrections folded into the remaining-B1 unit: (1) locators are now an
injective, reversible byte encoding (percent-escaped native path bytes) —
distinct non-UTF-8 sibling paths whose display strings collide keep
distinct ArtifactIds (Linux test), and lineage() obtains paths from the
inventory map instead of reopening the lossy locator string; the non-UTF-8
round-trip test now asserts the archive tier too (making the earlier doc
claim true). (2) The walker accepts regular files only (a matching FIFO
could block indefinitely — tested via mkfifo), and the symlink policy is
now explicit: the tree ROOT may be a symlink (relocated storage,
tested-supported), nothing within the tree is ever followed. (3) The
real-corpus completeness check is race-resilient: a count mismatch with a
changed artifact revision retries once and then counts as an aggregate
"raced" result rather than a correctness failure. (4) Compression
acceptance is fixture-proven: a committed external .zst (system zstd CLI
v1.5.4, XXH64 checksum) decodes identically to its plain twin; corrupting
its trailing checksum is rejected; a committed windowLog=28 frame (286 MiB
declared, 9 KiB on disk) is refused by the window guard before any
decompression. Fixture corpus at tests/fixtures/codex/ with manifest.json
per the round-11 policy (synthetic envelope + spec-synthesized legacy +
two external-CLI zst fixtures). Corpus re-run: 224/224, 0 errors,
0 violations, 0 count mismatches, 0 raced. 24 codex tests + conformance.

## Review round 14 (2026-07-17, same Codex agent) + B1 closing unit

Corrections: (1) the collision/lineage test was hollow (different thread
ids and timestamps meant pre-fix locators already differed; lineage was
called without asserting an edge) — reworked with IDENTICAL filenames under
both non-UTF-8 dirs (one logical session, two distinct artifacts, divergent
two-frame archive verified) and a non-UTF-8 fork fixture asserting the
exact Fork edge; (2) Windows locators encode native u16 units via
encode_wide (to_string_lossy collapsed distinct unpaired surrogates;
windows-only injectivity test added, runs in the CI matrix); (3) fixture
assertions tightened — the window-28 disposition must name the
window/memory refusal (decompress-then-JSON-fail would not pass), the
checksum rejection must name the checksum (observed: for sub-buffer frames
libzstd verifies before yielding output, so zero records emerge — recorded
in the manifest), and the manifest documents that the generated
active-truncation unit fixture satisfies the round-11 requirement.

B1 closing capabilities: (a) SourceProvider::parse_cache_token — the
aggregate token (schema version + full sorted descriptor state via
descriptor_state_token + every parse-policy input) implemented by all
three providers; unit tests prove a preferred-artifact flip with identical
revision text changes the token, and different safety limits never share a
token. (b) CodexProvider::drift_report — inspects NATIVE envelope/payload
vocabulary directly (B1's intentional Unknown dispositions are not drift):
known-vocabulary baselines from rust-v0.144.5, unknown-type counts at all
three levels, reasoning-summary availability measurement. Real-corpus run:
0 unknown vocabulary anywhere (the research vocabulary is complete for
this corpus) and reasoning summaries 24779/27765 (~89%), independently
reproducing the design-round census. 58 provider tests total.

## Review round 15 (2026-07-17, same Codex agent — B1 exit audit)

Three contract failures fixed, each with a test that fails against 48513e3:
(1) parse() reconstructed record artifact ids from lossy path display — on
non-UTF-8 homes every RecordRef named a nonexistent artifact; the id now
comes from the resolved descriptor, validate_provenance() rejects any
disposition/origin whose artifact is absent from descriptor.artifacts
(hostile forged-reference test), and the non-UTF-8 test asserts membership
for every RecordRef. (2) descriptor_state_token's \x1f/\x1e joins were the
same delimiter-collision class fixed in session/entry ids — replaced with a
canonical length-prefixed encoding including the COMPLETE preferred
ArtifactId; an adversarial test collides under the old encoding, and
parse_cache_token is exercised end-to-end through the provider-keyed cache
(stricter limits never share a cached parse). (3) drift_report now meets
the documented contract: unknown NESTED field paths against curated
baselines (the committed nested_future_field reports at its exact path),
unterminated active tails classified transient (never permanent drift),
era-bucketed reasoning availability by month (the aggregate-only ratio was
exactly the original research error), missing/malformed type
discriminators counted, and one unreadable session never suppresses
healthy results. EXPLICIT RE-PHASING: CLI `snatch doctor` surfacing and a
provider-neutral diagnostics hook are Phase B2 scope — B1 delivers the
analysis capability only.

Instrument validation: the nested-field scan's first real-corpus run
DISCOVERED vocabulary the source research missed ("metadata" on
message/function_call/reasoning and reasoning's metadata passthrough —
2,339 occurrences), absorbed into the baselines with that provenance;
corpus now reports 0 unknown paths, 9 era buckets, 0 active tails,
0 missing discriminators, 0 unreadable. Process note (owned): rounds 12-14
exhibited requirement evaporation — "doctor drift surfacing" was reported
complete while delivering type counts plus one aggregate ratio; this round
restores the documented scope and the re-phasing above states what remains
honestly. 66 provider tests.

## Review round 16 (2026-07-17, same Codex agent — B1 closing amendment)

Three fixes: (1) archived-artifact malformed tails are permanent corruption,
not transient active tails — classification now requires the preferred
artifact be non-archived (identical damage tested under sessions/ vs
archived_sessions/). (2) Drift coverage is machine-visible: "zero unknown
nested paths" was overstating a six-variant baseline — the report now
carries field_schema_checked_records, unbaselined_payload_types (kind/type
counts), and missing_payload_discriminators (payload-level `type`
missing/non-string for response_item/event_msg — the envelope counter did
not cover these), with baselines expanded to 14 variants where the source
schema is stable and the conformance output phrased as "no drift among X
checked; Y variants (Z records) NOT checked". The expanded run immediately
discovered two more real fields (agent_message memory_citation + phase,
2,992 occurrences) — absorbed with instrument-discovery provenance;
corpus now: 0 unknown paths among 190,826 checked records, 11 unbaselined
variants (1,240 records) honestly excluded. (3) The cache-policy tests were
hollow — the "strict" provider used a second fixture whose token differed
for root/locator reasons alone; both tests now construct the strict
provider over the SAME codex_home so the only changed input is the policy,
and the unreadable-session test asserts the count exactly (which exposed
that garbage .zst bytes error lazily on first read — such sessions are
scanned-with-unparseable-records, while open-time failures like the
compressed-input cap are the genuine unreadable path; both documented in
the test).

B2 guardrail recorded: unknown field names are native, attacker-controlled
strings — before `snatch doctor` prints them, cap distinct-path cardinality
and path length, track overflow counts, and emit no session ids, paths, or
field values by default.

## Review round 17 (2026-07-17, same Codex agent — B1 SIGN-OFF)

Verdict: "sign off B1 … proceed directly into B2. No further B1 architecture
or adapter review is needed." Reviewer independently reran the 38 applicable
provider tests, the forged-artifact provenance test, and the opt-in corpus
conformance test (224/224, zero errors/violations/mismatches/races; 190,846
checked records, 11 unbaselined variants / 1,242 records explicitly reported,
zero unknown checked paths — small count growth normal for an active corpus)
and confirmed each round-16 fix bites.

One doc-only correction ordered as the first B2 action (applied in this
commit): the consolidated checklist's claim to contain every forward
obligation was not literally true — (a) B2 was missing the
unqualified-prefix-uniqueness rule and the real-session milestone for
list/info/native/raw export; (b) B3 recorded only fork reconstruction where
the original plan requires typed fork AND spawn lineage; (c) doctor
responsibility appeared in both B2 and C without a boundary (now: B2 exposes
the provider-neutral drift report, C tunes presentation only). Reviewer also
added B2 acceptance guardrails, all folded into the checklist:
provider-selection resolution matrix (never silently fall back to Claude),
explicit `--provider all` partial-vs-atomic semantics, deterministic
cross-provider ordering, doctor caps applied during collection with
control-character escaping, a provider registry/shared resolver instead of
Codex-specific conditionals, and a cache-consumer test spanning an artifact
revision change between lookups.

## Execution checklist: B2 through D (consolidated 2026-07-17)

Single forward-looking list gathering every obligation accumulated across
review rounds 1-17 — read THIS (not just the phase prose) before starting a
phase, and check items off against their original wording (see the
requirement-evaporation memory: deferred parts must be named in the same
breath as any completion claim).

### Phase B2 — provider UX + production routing
- [x] `--provider claude-code|codex|all` (repeatable) on CLI; MCP requests
      gain optional `provider`; responses always carry provider + qualified
      session id. Default stays Claude-only until D.
- [x] Qualified-id parsing: FromStr/decoding + round-trip tests for the
      escaped encoding BEFORE ids become CLI/MCP inputs (round-6 guardrail).
      Unqualified prefixes are allowed only when unique across the selected
      providers; ambiguity is an error listing the candidates (phase-plan
      original wording, restored round-17).
- [x] Provider-selection resolution matrix, specified and tested as a matrix
      (round-17): repeated `--provider` flags; `all` mixed with explicit
      providers; a qualified id naming a provider outside the selected set;
      ambiguous unqualified prefixes. Never silently fall back to Claude.
- [x] `--provider all` availability semantics defined explicitly (round-17):
      whether one unavailable provider yields partial results (with a
      diagnostic) or fails atomically — pick one and test it.
      ROUND-18 STRENGTHENING (B2.9): the rule covers RUNTIME failures too —
      constructed providers whose `sessions()`/`diagnostics()` fail at call
      time are atomic under explicit selections and skipped-but-reported
      under `all` (listing/diagnostics), and UNQUALIFIED resolution under
      `all` refuses to choose while ANY provider (construction-skipped or
      runtime-failed) went unsearched — one hit elsewhere cannot prove
      uniqueness; qualified references pin their provider and are exempt.
      Tested with a hostile provider that constructs fine and fails at
      runtime.
- [x] Deterministic cross-provider ordering for any merged listing/output
      (round-17).
- [x] `snatch providers` command: discovered roots, session counts, format
      families, diagnostics.
- [x] Production routing through SourceProvider for every B3 consumer: shared resolver path
      SHIPPED (ProviderRegistry + resolution matrix; all provider-aware
      surfaces route through it, zero Codex conditionals at call sites);
      archive/native/raw methods gained production callers for BOTH
      providers (export tiers). B3 closes the tracked remainder for its
      consumer set: normalized exports, CLI+MCP messages/timeline, and the
      library API route qualified ids through cached ParsedSession bundles.
      Classic flagless inputs deliberately retain their established direct
      Claude path under invariant #8; later analysis/project surfaces migrate
      only when they gain provider scope in C/D, per the per-surface policy.
- [x] Parsed-session propagation: centralized
      `Conversation::from_parsed_session(...)` so provenance, semantics, and
      source cannot be independently forgotten; per-surface source threading
      lands WITH each surface's provider parameter (covers CLI + MCP +
      library/API — the ~28-site deferral inventory, rounds 10/T3).
      ROUND-18 CORRECTION: the first implementation ticked this while
      caching/bridging only `Vec<LogEntry>` — propagation was illusory.
      B2.7 makes it real: the cache retains `Arc<ParsedSession>` complete
      (`cached_parsed_session` / `get_or_parse_provider_session`);
      `from_parsed_session` RETAINS the bundle on the `Conversation` with a
      defined survival rule (bundle authoritative; node tree is a view;
      uuid→EntryId correlation is keep-first, matching dedup) and exposes
      `provider_bundle`/`entry_id_for_uuid`/`semantics_for_uuid`; the CLI
      and MCP info consumers construct through it and surface disposition
      counts + semantics counts; tests prove semantics/provenance survive
      both cache miss and hit.
- [x] First production cache consumer uses `parse_cache_token` (round-11
      guardrail; token already implemented + tested end-to-end). Test the
      consumer with an artifact revision change BETWEEN two lookups —
      stale-hit prevention, not just hit/miss (round-17).
- [x] `snatch doctor` surfacing of CodexDriftReport + a provider-neutral
      diagnostics hook (round-15 re-phasing). Boundary vs Phase C
      (round-17): B2 exposes the native provider-neutral drift report; C
      tunes semantic/presentation behavior (era bucketing display, etc.) —
      it does not "surface" it again. SECURITY (round-16/17): unknown field
      names are native attacker-controlled strings — cap distinct-path
      cardinality and path length DURING COLLECTION (not merely at
      rendering), track overflow counts, escape control characters to
      prevent terminal/structured-output injection, emit no session
      ids/paths/field values by default.
- [x] `codex` feature becomes default-on at release (round-11).
- [x] Compatibility promise from B2 on: backward-compatible inputs/semantics,
      additive provider metadata permitted; Claude raw-jsonl byte-identical
      permanently (invariant #8 phasing). Honored through B2.10: flagless
      CLI/MCP outputs unchanged, MCP response fields additive, MCP
      `limit: 0` semantics preserved on provider routes (round-19
      blocker 3). This is a STANDING promise, not a one-time deliverable —
      it stays binding through B3+.
- [x] Milestone (phase-plan original wording, restored round-17): list/info
      + native/raw export work on REAL Codex sessions — a real-session
      demonstration, not fixtures only.

## Slice exit protocol (standing, added round 22 — process countermeasure)

The recurring review-cycle failure class was consistent: semantic claims
shipped on confirmatory fixtures while the reviewer audited adversarially
against the real corpus. The audit style now lives in OUR exit gate.
Before claiming any normalization slice (or comparable semantic unit)
complete:

1. **Adversarial census first**: mine the corpus for the slice's FAILURE
   shapes — not "does my rule's precondition hold" but "enumerate every
   case my rule would misclassify". The census questions must attack the
   rule, not confirm it.
2. **Corpus-level invariant assertions**: every semantic rule ships with an
   aggregate assertion over all real sessions inside the opt-in
   conformance test (proven twins, reconciling sums, no-loss counts) — the
   invariant is enforced on the corpus, not just on fixtures.
3. **Fixtures are mined, not invented**: each fixture cites the observed
   corpus shape it reproduces; an idealized fixture proves only itself.
4. **Claims enumerate exclusions**: any "X is complete" names what is NOT
   covered in the same sentence.
5. **Reused machinery is re-audited**: before wiring existing analysis code
   to a new provider, list its semantic assumptions and check each against
   the new provider's data (the Claude-shaped turn pairing and
   is_human_prompt failures were this class).
6. **Oracles are source-derived** (round-23): a corpus assertion must
   compute its expectations from NATIVE records with independently written
   rule code — an oracle that reads the implementation's outputs and
   replays its formula proves only internal consistency.
7. **Cross-provider regression checks** (round-23): any surface behavior
   keyed on provider properties ships with a test that the OTHER
   provider's route is unchanged (the semantic-rendering flag silently
   regressed provider-routed Claude sessions).
8. **Oracles are mutation-tested** (round-25, BINDING): a green corpus is
   evidence about the implementation ONLY after the oracle itself has been
   proven to reject representative broken outputs. Every semantic oracle
   ships with deliberately-altered negative controls: start from a valid
   parsed object, alter exactly ONE property, and assert the specific
   violation. A source-derived oracle (rule 6) that is not mutation-tested
   can still be silently self-confirming — see the usage-allocation audit
   whose "attributed/preserved" partitions were both read from the
   implementation's own output. Enforcement lives in executable tests
   (`nc_*` in `src/provider/codex.rs`), not in this document. When claiming
   a slice complete, report BOTH the positive evidence (valid passes) and
   the negative-control evidence (each altered case fails for its reason).

## B3 slice 1 — normalization mapping (empirical, corpus of 224 real sessions)

Corpus facts the mapping rests on (2026-07-17 census): 202 dual-stream
sessions (response_item + event_msg content), 22 response_item-only, ZERO
event-only; `turn_id` lives in `turn_context.payload` (newer era),
`task_started`/`task_complete` events, and per-item
`internal_chat_message_metadata_passthrough`; `token_count.info` carries
`last_token_usage` (per-request delta) and `total_token_usage`
(session-cumulative) side by side, `info: null` heartbeats exist;
`event_msg user_message` fires only for GENUINE human prompts while
harness-injected context arrives as response_item user/developer messages.

Mapping (each mapped record keeps its B1 id `(ordinal, 0)` — constraint 1):

- response_item message role=assistant → `LogEntry::Assistant`
  (output_text → Text; unknown block types preserved as block-level
  Unknown); model from last `turn_context.model` (else "unknown").
- response_item message role=user/developer → `LogEntry::User` (input_text
  → Text); PromptSemantics Harness/TurnBoundary, upgraded to
  Human/TurnBoundary when a `user_message` event PROVED it as its twin in
  the pre-computed match plan (B3.1.1 — the earlier "nearest preceding
  unclaimed" claimant is gone).
- response_item reasoning → `LogEntry::Assistant` with a ThinkingBlock
  (summary + content texts; `encrypted_content` string → signature).
- response_item function_call / custom_tool_call → `LogEntry::Assistant`
  with ToolUse{id: call_id, name, input} (function_call `arguments` parsed
  as JSON, else raw string; custom_tool_call `input` as string);
  ToolSemantics{kind classified from name, native_name} keyed by call_id.
- response_item function_call_output / custom_tool_call_output →
  `LogEntry::User` with ToolResult{tool_use_id: call_id, output text};
  PromptSemantics Tool/MidTurn.
- event_msg user_message / agent_message / agent_reasoning /
  agent_reasoning_raw_content → B3.1 (round-22): suppressed ONLY when a
  one-to-one twin is PROVEN — matching is scoped to a turn window
  (session_meta/turn_context/task_started/compacted boundaries), each
  response_item (or reasoning SECTION — reasoning events are per-section)
  is claimable once, content agreement confirms the positional pairing,
  and the suppression records the twin
  (`DuplicateStream { twin_ordinal }` then; a full `RecordRef` since
  round 23). Unmatched event content maps
  directly (corpus: post-compaction "Compact task completed" notices,
  reasoning before aborted turns — 380 such events were being discarded).
  The matched `user_message` event marks exactly its twin Human (the
  former LIFO claimant could mis-mark harness entries). B3.1.1 (round-23):
  identical native events — same payload type, payload JSON, timestamp,
  and window — are ONE semantic emission: later copies suppress against
  the representative's target (fingerprint includes the timestamp, so
  repeated text at a different time stays distinct); suppression twins are
  full `RecordRef`s validated as MAPPED records by `validate_provenance`;
  and an unmatched non-duplicate `user_message` maps as Human/**MidTurn**
  (steering-or-unknown), never opening a turn boundary automatically.
- event_msg token_count with usage → canonical usage derives from
  CUMULATIVE transitions (unchanged totals → zero; input/output decrease →
  epoch reset whose new cumulative is the first delta) — NEVER a blind
  `last_token_usage` sum (94/178 real sessions disproved that, round 22).
  Records attach N:1 to the current window's assistant emission; events
  arriving BEFORE their response are held for the next assistant WITHIN
  the window (round-23: pending usage never crosses a boundary — it
  flushes as preserved/unattributed); with no assistant, the record stays
  a preserved Unknown entry (never lost). B3.1.2 (round-24, superseding
  the round-23 "era" theory): the basis is SOURCE-BACKED — Codex's own
  TokenUsage defines non-cached input as input − cached across every
  audited tag (0.31…0.144.5), and the census (61,528 observations) found
  ZERO contradicting cumulative points. The basis is validated PER
  OBSERVATION: an observation whose own numbers contradict it (cached >
  input — four real Call observations in one January session) is marked
  Unknown/ambiguous with raw values preserved, never reinterpreting the
  session. Ambiguity is FIELD-SPECIFIC: an uninterpretable fresh decrease
  zeroes only the FRESH delta (cached/output still contribute) and flags
  the Cumulative observation. Observations carry raw native numbers in
  dedicated fields plus their own `RecordRef` and basis. The conformance
  oracle is SOURCE-DERIVED and strengthened (round-24): attribution from
  each observation's own RecordRef; XOR partition (every native usage
  record in exactly one of attributed/preserved); observation values
  verified per aggregation (Call↔last, Session↔total) with basis and
  ambiguity recomputed independently; dedup twins verified STRUCTURALLY
  (type correspondence + exact extracted content, or exact fingerprint
  for event-to-event duplicates — no empty-text escape).
- session_meta, turn_context, task_started/complete and all other unmapped
  types → remain Unknown entries (preserved verbatim; consumed as
  normalization STATE — version, cwd, model, turn_id — without changing
  their disposition). Slice 3 adds fork/spawn lineage and inherited-history
  activity. Slice 4 maps each `compacted` record once as a system boundary
  while keeping replacement history nested, and annotates verbatim
  world_state/ghost_snapshot Unknown entries as reconstruction state.
- turn_id → `EntrySemantics.turn_id` (new sidecar field — separate carrier,
  never message identity; constraint 2): ambient turn_context/task_started
  state, OVERRIDDEN per item by its own
  `internal_chat_message_metadata_passthrough`/`metadata` carrier (B3.1:
  honored, not merely coincident with ambient).
- Synthetic linear threading: mapped entries get the INJECTIVE EntryId
  encoding as their uuid (provider+namespace+native+ordinal; B3.1 — a bare
  `native:ordinal` would collide across providers/namespaces once exports
  make it sticky), parent = previous mapped entry.
- response_item web_search_call → assistant ToolUse (kind Web) — B3.1;
  341 corpus records.
- Surfaces (B3.1): provider timeline groups turns SEMANTICALLY (turn_id
  transitions + Human TURN-BOUNDARY prompts — round-24: MidTurn/steering
  human prompts stay inside their turn, honoring PromptDelivery; harness
  preambles form no turn),
  and provider messages uses PromptSemantics for its human predicate — the
  Claude-shaped adjacent-pairing/is_human_prompt heuristics mislabeled
  harness context (a one-task real session reported 77 turns; now 1).

## B3 slice 2 — same-turn steering presentation (2026-07-17)

Codex's documented interaction model distinguishes steering (append input to
the active turn) from queuing (save input for the next turn); app-server's
`turn/steer` likewise appends input without creating a new turn. Sources:
[Codex prompting — steering and queuing](https://learn.chatgpt.com/docs/prompting.md)
and [official app-server protocol](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md#example-steer-an-active-turn).

An independent census over the preferred artifacts of 226 sessions found
1,709 native `event_msg.user_message` records: 1,705 claim-once exact-content
pairs with response-item user prompts, two exact duplicate events, and two
unique same-window additions. Of the two additions, one precedes the first
assistant emission and one occurs between assistant emissions; both use the
current `images,local_images,message,text_elements,type` payload shape. No
window contains more than one unique addition. These are represented as
Human/MidTurn steering while paired prompts remain Human/TurnBoundary.

Presentation is additive: a separate provider-aware `SemanticTurn` retains
`steering_messages` in native order (the public legacy `ConversationTurn`
stays source-compatible); timeline text renders `Steering: ...`, and JSON adds
`steering_prompts` only when non-empty, so Claude output remains unchanged.
The corpus conformance test independently derives the expected boundary vs
midturn partition from native records, requires both steering records to
survive semantic grouping and timeline rendering, and reports only aggregate
counts. Mutation controls prove the oracle rejects a steering prompt relabeled
as a boundary and a harness prompt relabeled human. The two observed placement
shapes are covered between the real-corpus gate and a mined integration
fixture. Exclusion stated explicitly: no distinct native "queued" discriminator
was observed; queued input is represented as its later ordinary turn-boundary
prompt, consistent with the documented next-turn behavior.

## B3 slice 3 — fork history and typed lineage (2026-07-17)

The current Codex source makes the relationships explicit: `SessionMeta`
has distinct `forked_from_id` and `parent_thread_id` fields, and a spawned
thread's typed source is
`{subagent:{thread_spawn:{parent_thread_id,depth,agent_role,...}}}`. Older
fork construction instead derives `forked_from_id` from the copied initial
history's first `SessionMeta`. Sources:
[Codex protocol at rust-v0.144.5](https://github.com/openai/codex/blob/rust-v0.144.5/codex-rs/protocol/src/protocol.rs#L2497-L2521)
and
[current SessionMeta/SubAgentSource definitions](https://github.com/openai/codex/blob/rust-v0.144.5/codex-rs/protocol/src/protocol.rs#L2786-L2804).

The independent corpus census found 16 two-meta rollouts among 226 preferred
sessions. Every one has exactly the strict old-format shape: physical record
zero is metadata for the filename thread, record one is metadata for a
different existing thread, and the second metadata plus following records
form an exact prefix of that parent's preferred rollout when ONLY the outer
envelope timestamp is ignored. No current-corpus metadata carries
`forked_from_id`, `parent_thread_id`, or a linkable `thread_spawn` source, so
the 16 observed edges are forks; the modern spawn shape is pinned by an
official-source-derived adversarial fixture rather than falsely claimed as
corpus-observed.

Implementation: `lineage()` emits typed `Fork` edges from the modern direct
field or the strict physical-record-one heuristic, and typed `Spawn` edges
from the direct/nested parent carrier with the recorded agent role. Later
second metadata never qualifies. During parse, the provider proves the
maximal copied prefix against the available parent using complete-envelope
equality except for the rewritten outer timestamp. Its end is a hard
dedup/usage window boundary, and every normalized entry whose producing
records lie wholly inside the prefix is annotated
`ActivityKind::InheritedHistory`; copied records stay present in the fork
view but are excluded from new-work projections. A missing parent leaves a
dangling edge but causes no guessed activity classification—there is then no
available parent session with which to double-count.

The child's parse-cache token includes the parent descriptor state, including
the missing→present transition, because inherited classification depends on
it. The real-corpus gate independently reconstructs both the complete lineage
edge set and each copied prefix, then audits the exact inherited/new activity
partition and forbids producing/dedup edges crossing the boundary. Mutation
controls flip one inherited entry to new and one new entry to inherited; both
must fail. A separate fixture leaves copied usage pending at the boundary and
proves it is preserved as inherited rather than attached to the fork's first
new assistant.

## B3 slice 4 — compaction and reconstruction state (2026-07-17)

Official Codex reconstruction semantics, rather than normalized output,
define this mapping. A `compacted` rollout item replaces model history from
its optional `replacement_history`; it does not append each replacement item
as new chronological activity. Window metadata carries a modern UUIDv7 chain
(`first_window_id`, `previous_window_id`, `window_id`) plus a sequential
window number; older rollouts encode the numeric position directly in
`window_id`. A full `world_state` establishes a reconstruction baseline and a
non-full item is an RFC 7386 merge patch. Legacy `ghost_snapshot` response
items are filtered from model history by Codex's rollout loader. Sources:
[Codex protocol at rust-v0.144.5](https://github.com/openai/codex/blob/rust-v0.144.5/codex-rs/protocol/src/protocol.rs)
and
[Codex rollout reconstruction at rust-v0.144.5](https://github.com/openai/codex/tree/rust-v0.144.5/codex-rs/core/src/rollout/).

Implementation: each physical `compacted` record maps to exactly one
`System/CompactBoundary` entry with its summary as content. Every other native
payload field, including the complete `replacement_history`, remains nested
in `SystemMessage.extra`; nested messages, ghost snapshots, and compaction
markers are NEVER expanded into entries or counted as activity. Typed
`CompactionSemantics` records replacement cardinality and the normalized
`CompactionWindow` (including whether a numeric legacy id supplied the
number). `world_state` and top-level `ghost_snapshot` stay exact
`LogEntry::Unknown` values with typed `StateCheckpointKind` sidecars because
they are persisted reconstruction state, not user/assistant emissions.

The independent 226-session census now enforced by conformance finds 58
boundaries, 951 nested replacement items, 42 full world-state baselines, one
world-state patch, and 87 top-level legacy ghost checkpoints. It derives the
expected entry, complete nested payload, carrier, cardinality, window chain,
and origin from native records. Mutation controls prove rejection of a wrong
replacement count, dropped nested history, illicit expansion of one nested
item even when generic 1:N provenance remains valid, a wrong state kind, and
semantic carriers placed on incompatible entry types. Exclusion: this slice
preserves and types reconstruction state but does not render world-state
contents or replacement items; compaction-window presentation remains Phase C.

## B3 slice 5 — normalized exports and semantic-coverage gate (2026-07-17)

Provider-routed normalized exports now parse through the provider-keyed cache,
construct via `Conversation::from_parsed_session`, and use the existing common
exporters and single redaction/filter transform. Markdown, JSON, pretty JSON,
JSONL, text, CSV, HTML, and SQLite are supported; source-fidelity raw/native/
archive tiers remain separate and reject every transform. Normalized routes
support the portable content, presentation, PII-warning, redaction, and output
flags while explicitly refusing Claude-only chain/subagent/persisted-output
machinery and external gist/clipboard/template delivery. File output remains
preflighted and atomic, including SQLite via an adjacent temporary database.
An integration matrix renders every format, verifies redaction, opens the
SQLite result, and pins provider-routed Claude markdown byte-for-byte against
the classic path.

The real-corpus conformance gate now inventories every `Unknown` disposition by
native envelope/payload family and rejects any family not explicitly classified.
All current `response_item` content families are mapped except the deliberately
non-model `ghost_snapshot`. The remaining Unknowns are session/turn/world-state
metadata; review, rollback, settings, task, abort, command/patch/search lifecycle
events; and token-count records with no defensible assistant owner. These stay
content-complete as exact native values but are not fabricated into semantic
emissions. A new future family therefore makes conformance red until it is
mapped or consciously classified rather than disappearing inside an aggregate
Unknown count.

## B3 slice 6 — API and MCP bundle routing (2026-07-17)

The last B3 consumer-routing deferral is closed. `SnatchClient` accepts
explicit Claude/Codex roots for embedded callers; existing parse, conversation,
analytics, comparison, and string-export methods recognize provider-qualified
ids while unqualified behavior stays Claude-only. The new
`parse_provider_session` API exposes the complete `Arc<ParsedSession>` rather
than forcing callers to discard provenance and semantics. Its integration test
proves identity/provenance survive into `Conversation`, normalized export, and
qualified session info without process-global environment mutation.

MCP `get_session_messages` and `get_session_timeline` now accept an optional
provider selection or a qualified id, resolve through the registry/cache, and
return additive `provider` + `qualified_id` fields. Semantic providers use
PromptSemantics and semantic turns rather than Claude-shaped human/turn
heuristics; same-turn steering is retained in the timeline response and
compaction boundaries remain visible. Provider routes explicitly refuse chain,
subagent-transcript, and chunk controls until the corresponding provider-neutral
consumer exists. A real-shape Codex MCP fixture proves one semantic turn,
midturn steering, compaction reporting, qualified identity, and refusal of a
Claude-only chunk request. Classic requests remain on their existing path and
the full suite pins compatibility.

## B3 slice 7 — exported derivation metadata (2026-07-17)

The B3 exit audit caught one acceptance-invariant gap despite the green corpus:
normalized JSON/JSONL carried synthesized Claude-shaped linkage fields but did
not yet tell downstream machines which fields were adapter-derived. That would
leave consumers free to mistake Codex ordering links for native causality.

`ParsedSession` now declares typed session-level `FieldDerivation`s. Codex
marks `uuid` and `message.id` as deterministic EntryId encodings, and
`parentUuid`/`logicalParentUuid` as links to the previous normalized emission
(stable ordering, explicitly not native causality); Claude declares none.
Normalized JSON adds an optional versioned `provider` envelope containing the
qualified identity, those derivations, record-accounting totals, and each
rendered entry's deterministic id + native origins. JSONL uses a versioned
metadata header followed by wrapped entry lines carrying the same per-entry
derivation. Classic flagless exports have no ParsedSession bundle and retain
their established shapes.

Artifact paths are intentionally absent: origins use deterministic export-local
labels (`artifact-0`, ...) so provenance remains useful without leaking source
locators or smuggling native content around redaction. Uuid-less exact Unknown
state records are correlated by the parsed-session bridge's parallel orphan-id
vector, never by content equality. Production acquisition validates the full
provenance graph before caching, and JSON/JSONL refuse invalid or unidentified
provider entries. Integration coverage proves derived-field declarations,
entry/origin completeness (including a uuid-less record), versioned JSONL
wrappers, locator non-disclosure, and redaction with metadata retained.

## Phase B3 exit audit (2026-07-17)

**Result: B3 complete; C/D deliberately remain open.** The consolidated B3
checklist was re-read against its original wording after slice 7, not inferred
from aggregate green tests. Each item has a production consumer and biting
coverage: mapped content + exact deterministic ids; ambient/per-item turn ids;
emission-identity dedup and source-derived usage allocation; turn-boundary vs
midturn prompt delivery; typed fork/spawn lineage and inherited-history
projection; compaction/state carriers; complete-bundle CLI/MCP/API routes; all
normalized export formats; and machine-readable JSON/JSONL derivation.

The exit evidence is:

- Full `just ci`: 922 unit tests, 75 CLI integration tests, corpus/property/
  snapshot suites, clippy, doc tests, and rustdoc all green. Existing flagless
  Claude snapshots remain unchanged; Claude raw-jsonl remains byte-identical.
- Opt-in native conformance: 226/226 sessions parsed; zero errors, provenance
  violations, count mismatches, or raced sessions. Record accounting:
  163,886 mapped, 30,251 intentionally suppressed, 32,087 preserved Unknown,
  0 recovered, 2 unparseable.
- Semantic census: 1,707 boundary prompts + 2 midturn steering prompts; 16
  copied-history/fork-lineage sessions; 59 compaction boundaries with 989
  nested replacement items; 43 full + 1 patch world states; 87 legacy ghost
  checkpoints. All allowed preserved-Unknown families are enumerated by the
  conformance gate; any new unclassified family fails it.
- Drift coverage states its boundary: zero unknown nested paths among 194,313
  checked records; 12 unbaselined variants (1,456 records) explicitly not
  checked; nine era buckets; zero active tails, missing discriminators, or
  unreadable sessions.

Acceptance invariants 1-3 and 5-8 are executable now. Invariant 4's
single-session pieces are executable (replacement history never expands;
inherited history has a new-work projection), while its actual cross-session
non-double-count claim remains intentionally gated on D's union view rather
than being claimed against a surface that does not exist yet.

Explicit non-B3 work, carried forward rather than evaporated: semantic prompt
chunking, lessons tuning, usage-observation consumers, reasoning/drift and
compaction-window presentation, and the unpriced policy are Phase C; unified
projects, default-provider policy, registry storage, and optional interchange
exports are Phase D. Pre-envelope parsing remains unsupported by decision;
legacy recognition/diagnostics/fidelity export are tested with the documented
synthetic fixture because this live corpus contains no legacy session.

## Review round 26 (2026-07-17, same Codex agent — B3.1.3 audit: B3.1.4)

Verdict: the preserve-all loophole is fixed and all ten controls are
substantive, but two wrong-attribution contracts still escaped the
ordinal-keyed audit. B3.1.4 (test + validator hardening): (1) the audit and
the generic `validate_provenance` now enforce FULL RecordRef identity —
usage observations must reference the preferred artifact and be an origin
of the annotated entry, so a same-ordinal SIBLING-artifact swap is caught
(new control `nc_observation_wrong_sibling_artifact_is_rejected` uses a real
two-artifact session; the validator's origin-correspondence check also
rejects it, membership alone would not); (2) canonical usage is reconciled
PER OWNER EntryId, not as one global sum — expected canonical is
accumulated onto each token's independently derived owner and compared
entry by entry, with entries expecting none required to carry none (new
control `nc_canonical_usage_moved_between_assistants_is_rejected` moves
usage between two assistants leaving the global sum unchanged; a global
check would pass, per-owner rejects). Twelve negative controls now;
positive control and 226-session corpus green under the stricter audit.

## Review round 25 (2026-07-17, same Codex agent — B3.1.2 audit: B3.1.3)

Verdict: production changes are materially correct, but the conformance
oracle still derived the expected usage allocation from normalized OUTPUT
(`attributed` from emitted observations, `preserved` from Unknown entries),
so a broken impl that preserved every token_count and emitted no
observations would pass. B3.1.3 (test-hardening only): the usage audit is
extracted into a reusable helper (`audit_usage_allocation`) that derives
the expected partition, owner, cardinality, values, basis, and ambiguity
from the NATIVE record stream alone (independent window walk, independent
event dedup for owner classification, source-backed includes-cached basis,
independent cumulative-ambiguity recomputation). Ten deliberately-altered
negative controls (`nc_*`) prove it rejects, each for its specific reason:
all-preserved-no-observations, missing Call, missing Session, duplicate
observations, swapped scope/aggregation, non-token_count source,
wrong-assistant (same window), cross-window attribution, both-partitions,
neither-partition. Cardinality uses Vec counts, not a set. Docs corrected:
`UsageBasis` is now provider-neutral with Codex's source-backed policy
documented separately and the retracted "excludes era" theory marked
do-not-reintroduce; `semantic_turns` says Human/TurnBoundary; the steering
test is renamed `midturn_steering_does_not_split_the_turn` and honestly
scoped to "does not split" with the presentation obligation preserved in
the B3 forward checklist. Slice-exit protocol gains BINDING rule 8
(mutation-tested oracles). Corpus after B3.1.3: 225/225, independent
allocation audit green on every session; negative controls all reject.

## Review round 24 (2026-07-17, same Codex agent — B3.1.1 audit: B3.1.2)

Verdict: one more bounded correction pass. All accepted and fixed:
(1) the timeline renderer ignored PromptDelivery — MidTurn human prompts
opened turns despite the normalizer's annotation; the flush is now
boundary-only (steering fixture: boundary prompt + assistant + steering +
assistant, one turn_id → ONE turn); (2) the round-23 "excludes era" was a
WRONG THEORY — the reviewer verified Codex's own TokenUsage source across
five tags and the census (61,528 observations, zero contradicting
cumulative points): the basis is a source-backed constant, validated per
observation (the four real contradictory Call observations go
Unknown/ambiguous with raw values preserved), session-level detection
deleted, the invented "excludes era" fixture replaced by the actual
last.cached > last.input corpus shape, and ambiguity documented as
field-specific; (3) the oracle's confirmatory shortcuts removed —
attribution from observation RecordRefs (not positional origins), XOR
partition, fixed source-backed basis rule, per-observation
value/basis/ambiguity verification against native records, structural
twin comparison with no empty-text escape; (4) the hollow web-search test
(an earlier unverified bulk edit had silently failed to replace it) now
asserts native id, status, action, and the id-less fallback. Corpus after
B3.1.2: 224/224, 0 violations, strengthened oracle green on every
session. Process note: the silent-edit failure is itself now guarded —
bulk text edits assert their application (this round's doc edits caught
two more stale anchors exactly this way).

## Review round 23 (2026-07-17, same Codex agent — B3.1 audit: B3.1.1)

Verdict: B3.1 materially improved but one more bounded unit. All accepted
and fixed (details in the amended bullets above and the protocol's new
rules 6-7): (1) semantic rendering keyed on a new
`ProviderCapabilities::semantic_annotations` capability — the
bundle-presence test had regressed provider-routed CLAUDE sessions (zero
prompts, collapsed timeline; cross-provider parity now tested); (2)
identical native events fingerprint-deduplicate into one emission, and
unmatched user events are Human/MidTurn, not turn boundaries; (3) pending
usage is window-scoped (boundary flushes to preserved — corpus
token→abort→assistant sequences no longer mis-attribute), and the
conformance oracle is source-derived instead of replaying the production
formula; (4) usage basis is explicit and detected (includes/excludes
cached — both statements were true, for different eras), ambiguous
transitions are surfaced not clamped, and observations carry raw native
numbers + their own RecordRef + basis (positional zipping eliminated);
integrity: `DuplicateStream { twin: RecordRef }` validated as a MAPPED
record by `validate_provenance` (forged-twin test), web_search_call
preserves its native `ws_...` id (158/341 corpus records; mined fixture),
stale claimant prose removed. Corpus after B3.1.1: 224/224, 0 violations,
source-derived oracle green on every session.

## Review round 22 (2026-07-17, same Codex agent — slice-1 audit: B3.1)

Verdict: normalization strategy sound; several implementation claims
DISPROVEN by the real corpus; one bounded B3.1 hardening unit, then re-run
the semantic audit. All findings accepted and fixed (see the amended
mapping bullets above): proven-twin dedup with twin ordinals in the
suppressions (380 unique events were being discarded, and the LIFO human
claimant could mis-mark harness entries); canonical usage from cumulative
transitions with held attribution and preserved orphans (blind last-usage
summing was wrong in 94/178 sessions; a further era with input-excluding-
cached was caught by OUR new corpus reconciliation audit); semantic
timeline turns + semantic human predicate (77-turn session → 1 turn);
per-item turn carrier honored; web_search_call mapped; injective synthetic
uuids. All seven demanded fixtures added (each citing its corpus shape),
plus standing conformance assertions: every DuplicateStream suppression
must point at a mapped twin, and canonical usage must reconcile against an
independent replay of the cumulative stream — on every session, every run.
Corpus after B3.1: 224/224, 0 violations, mapped 161,188 / suppressed
30,131 / preserved 31,730 / unparseable 2. This round also produced the
standing "Slice exit protocol" above.

## Review round 21 (2026-07-17, same Codex agent — B2 SIGN-OFF)

Verdict: "Yes—B2 is signed off. Proceed directly to B3." Reviewer
independently verified the B2.11 fixes (cache partition budgets incl.
oversized replacement + saturating arithmetic; doctor wholesale error
replacement with stdout+stderr adversarial tests; qualified-reference
rejection on --list-templates; status-triple coverage), the clean worktree,
the full local matrix (873 library tests + integration/corpus/property/
snapshot/doc + no-default-features + MCP-only), and green remote CI. The
documented [~] api/flagless routing remainder is accepted as explicitly
B3-consumer work.

HARD CONSTRAINTS for B3 (reviewer-set, binding):
1. Preserve existing deterministic EntryIds when records change from
   Unknown to Mapped.
2. Model turn_id separately; never repurpose message identity.
3. Deduplicate by semantic emission/call identity — not text equality —
   and PROVE token usage is not double-counted.
4. Keep fork-inherited history, compaction replacement history, forks, and
   spawns semantically distinct.
5. Route new consumers through the complete ParsedSession bundle so
   provenance and semantics are not stripped again.
6. Validate each normalization slice against both adversarial fixtures and
   the real corpus before expanding it.

Checkpoint: after the first end-to-end normalization slice —
user/assistant content, reasoning summaries, tool calls/results, and
usage — works through messages and timeline on real sessions.

## Review round 20 (2026-07-17, same Codex agent — narrowly conditional sign-off: B2.11)

Verdict: B2.10 passes except two exit defects + one edge; land B2.11 with
adversarial tests, then B2 is signed off and B3 begins without another
architecture review. All fixed:

1. **Oversized entries bypassed the cache budget**: the LRU inserted an
   item unconditionally once eviction emptied the cache, so a single item
   larger than its partition breached the budget (the round-19 flood test
   was a hollow-test variant — its "oversized" values coincidentally fit
   the aggregate and it never populated metadata). Now `insert`/
   `insert_keyed` REFUSE any item whose estimate exceeds the partition's
   `max_size` (stale same-identity values still removed first — an
   oversized replacement removes the old value and caches nothing), size
   arithmetic saturates, and the adversarial replacement test inserts
   oversized legacy AND provider values simultaneously with a populated
   metadata partition, asserting each partition's `current_size <=
   max_size` — not a coincidental aggregate.
2. **Doctor leaked paths on ERROR paths**: `provider_diagnostics`
   propagated collection errors verbatim (`?`), so the zero-success and
   atomic-explicit paths printed raw construction reasons including
   filesystem roots. Collection failures are now replaced WHOLESALE with a
   fixed public message (nothing interpolated, so nothing can leak);
   sentinel-path tests cover stdout+stderr across all-unavailable-at-
   construction, explicit runtime diagnostics failure, `all` with zero
   runtime successes, and partial success.
3. **Edge**: `export codex:... --list-templates` silently ignored the
   qualified reference — under the accepted policy a qualified id IS a
   provider request, so it is now rejected like `--provider`.
4. The `constructed`/`scan_ok`/`available` triple is pinned by committed
   integration assertions across all three states (available; not
   constructed; constructed-but-scan-failed via a sessions-tree-as-file
   codex home).

## Review round 19 (2026-07-17, same Codex agent — B2 re-audit: one bounded B2.10)

Verdict: B2.7–B2.9 pass their main-path review; four exit blockers + smaller
contract mismatches remain; land ONE bounded B2.10 closing amendment, then
B3 without another architecture review. All fixed in B2.10:

1. **Cache budget ~190%**: the two parsed caches each took 90% of
   `max_size`. Now 45%/45% (metadata keeps 10%), `total_entries`/
   `total_size` include provider bundles, and a test fills all three caches
   and proves the aggregate stays within the configured budget and that
   `clear()` empties everything.
2. **`--max-file-size 0` disabled Codex bomb guards**: zero is normalized
   to "no additional user cap" at the registry (and `tighten_limits(0)` is
   a no-op as defense in depth), so the built-in compressed/decompressed
   ceilings always stand; zero/omitted/huge produce identical provider
   state and identical cache tokens (canonical, no redundant variants).
   End-to-end CLI test: omitted and 0 parse; a small nonzero limit refuses.
3. **MCP `limit: 0` regressed on provider routes**: provider listing now
   ALWAYS truncates to the requested limit (0 = zero rows), with a parity
   test comparing classic vs provider routes at limits 0/1/999. (CLI keeps
   its own documented 0-is-unlimited convention on both routes.)
4. **Runtime `all` centralized**: `collect_selected_sessions` /
   `collect_selected_diagnostics` on the registry enforce
   atomic-under-explicit, partial-under-`all`, and error-on-zero-runtime-
   successes; CLI list, MCP list, and doctor are now thin renderers over
   them, and the contract is tested once at the registry.

Smaller items: `--context-length` added to the list refusal table;
`export --list-templates` rejects `--provider` (independent action, the
selection would be ignored); all four provider-route validators now
DESTRUCTURE their argument structs without `..`, so any future field is a
compile error until classified; the vocabulary cap now bounds the complete
stored key at exactly 120 chars including the truncation marker; `snatch
providers` reports `constructed`/`scan_ok`/`available` as three separate
fields with text derived from the same facts. Checklist honesty: the
production-routing item stays [~] DELIBERATELY — the remaining
`api.rs`/flagless-path migration lands alongside B3's per-surface
consumers, and B2 is not claimed fully complete while it remains.

## Review round 18 (2026-07-17, same Codex agent — B2 exit review: NOT ready)

Verdict: proceed with B2 HARDENING, not B3. Five blockers, all accepted and
fixed in three units:

- **B2.7 (parsed-session propagation was illusory)**: the cache retained
  only `Vec<LogEntry>` and `from_parsed_session` stripped the bundle — the
  checked-off propagation claim was the requirement-evaporation pattern
  again. Fixed: `Arc<ParsedSession>` cached complete
  (`cached_parsed_session`), `Conversation` retains the bundle with a
  defined survival rule (authoritative bundle, node-tree view, keep-first
  uuid→EntryId correlation) and semantics accessors; CLI/MCP info construct
  through the bridge and surface disposition/semantics counts; tests prove
  survival across cache miss and hit.
- **B2.8 (option loss, limit loss, split predicates)**: one qualification
  predicate (`looks_qualified`) used by every resolution path, with
  command-level tests for Windows paths, ghost prefixes, malformed escapes,
  and encoded-colon natives; ambiguity candidates sorted before truncation;
  `RegistryConfig` threads `--max-file-size` into both providers (Codex
  caps tightened, plain files now bounded by the same cap; limit changes
  the cache token and refuses oversized parses); COMPLETE table-driven
  option classification per provider route (list 21+targets, info 7,
  doctor 3, export 35 incl. security flags and default-true toggles, MCP
  scope args) with every unsupported argument individually refused and
  individually tested.
- **B2.9 (runtime `all`, diagnostics hardening, consistency, atomicity)**:
  runtime-failure semantics above; vocabulary length cap applies to the
  ESCAPED representation (300-control-char test); doctor withholds
  unavailability/failure detail entirely (no filesystem paths — `snatch
  providers` is the detail surface, and its available/scan-failed states
  are now consistent between text and JSON); provider exports preflight
  format+capability+artifact resolution BEFORE touching the destination
  and stream through AtomicFile (temp + rename); MCP tool descriptions and
  raw-jsonl CLI docs de-Claude-ified. Compatibility: flagless outputs
  unchanged; MCP response additions remain additive (invariant #8).

#### B2 status (2026-07-17, milestones 1-6 shipped)

Commits a7dffc7 (checklist amendment), 9ae773e (FromStr + registry +
`snatch providers`), bfda472 (selection + resolution matrix, 11 tests),
0587775 (`from_parsed_session` + keyed cache + `cached_session_entries`
with the revision-change test), aebfd07 (CLI list/info/export provider
routing, 10 integration tests, real-corpus demonstration), ad673a8 (MCP
list_sessions/get_session_info provider routing + always-on
provider/qualified_id response fields), 95176aa (doctor diagnostics hook,
collection-time security caps with hostile-input test, codex default-on).

Decisions taken in-implementation, FLAGGED FOR REVIEW:
1. Default policy for qualified ids (`resolve_with_default_policy`): with
   no `--provider` flag, an UNQUALIFIED reference stays Claude-only (the
   phase-plan default until D), but a QUALIFIED id (`codex:...`) is itself
   an explicit provider request and resolves against exactly the provider
   it names. Rationale: typing the provider's name is as explicit as the
   flag; no silent fallback exists in either direction. `looks_qualified`
   only treats a reference as qualified when its first segment names a
   REGISTERED provider, so Windows paths / legacy colon-bearing references
   still reach the classic path.
2. `--provider all` semantics: explicit selections are ATOMIC (any broken
   or unknown named provider fails the call); `all` is PARTIAL but never
   silent (skipped providers surfaced with reasons; zero working providers
   errors; not-found results name unsearched providers).
3. Prefix resolution: one exact native-id match wins over longer prefix
   matches; otherwise unique-or-error with qualified candidates listed.
4. Provider-neutral listing/info intentionally show identity + artifacts
   + honest entry-type counts only (no titles/timestamps) until B3
   normalization; Claude-specific filters are refused, not ignored.
5. Vocabulary caps: 64 distinct keys per drift map, 120 chars per key,
   dropped/truncated counters, `escape_debug` for control characters —
   applied in `bump_vocab` during collection.

### Phase B3 — Codex normalization
- [x] Content-bearing response records covered by B3.1 flip from
      Unknown{entries} to Mapped with the SAME deterministic ids (B1 parse
      comment contract). The slice-5 semantic-coverage gate enumerates every
      still-Unknown family corpus-wide and fails on any unclassified family;
      current survivors are intentional lifecycle/metadata/checkpoint records
      plus unattributable token counts, never an unexamined content bucket.
- [x] turn_id carrier before normalization (round-6 guardrail), including
      ambient and per-item carriers.
- [x] Two-stream dedup for the B3.1 content vocabulary under invariant #3's
      emission-identity rule: response_item authoritative when a proven twin
      exists; unique event content maps; usage reconciles independently.
- [x] Steered/queued prompt shape and presentation (B3 slice 2): paired native
      prompts are Human/TurnBoundary; the two corpus-observed unique same-turn
      additions are Human/MidTurn and render once inside their existing turn.
      Queue has no observed native discriminator and remains an ordinary later
      turn boundary, matching Codex's documented next-turn semantics.
- [x] `world_state` / `ghost_snapshot` semantics — exact Unknown state
      records with typed full/patch/legacy-ghost checkpoint carriers (B3
      slice 4), source-derived and mutation-tested.
- [x] Typed fork AND spawn lineage (phase-plan original wording, restored
      round-17): fork reconstruction via the embedded-second-meta heuristic
      (this corpus's forks predate forked_from_id — B1a observation) AND
      typed `Spawn` edges from Codex subagent `parent_thread_id`/source
      metadata — both as LineageEdge kinds, not fork alone. Slice 3 also
      proves copied parent prefixes and marks their entries InheritedHistory.
- [x] Compaction: each `compacted` record maps once as a chronological system
      boundary; replacement_history stays nested and is never counted as new
      activity (invariant #4); typed modern/legacy window metadata ships (B3
      slice 4), source-derived and mutation-tested.
- [x] Semantic sidecar emission: prompt axes, per-call tools, turn ids,
      valued usage observations, InheritedHistory for fork-copied records,
      compaction windows, and reconstruction-state checkpoints ship.
- [x] Pre-envelope legacy files: keep unsupported-legacy refusal unless
      provenance-documented fixtures justify a parser (round-6 posture).
- [x] Milestone: messages and timeline work on real Codex sessions, including
      steering; normalized markdown/JSON/JSONL/text/CSV/HTML/SQLite exports
      route through the complete ParsedSession bundle and common transforms.

### Phase C — semantic tuning
- [x] Codex prompt-boundary chunking (Phase C slice 1): Human/TurnBoundary
      prompts start chunks; Human/MidTurn steering stays inside the active
      chunk, while pre-boundary steering remains explicit preamble. Messages,
      chunks, and MCP use the same semantic rule. A source-derived corpus
      audit proves the exact boundary set and steering membership across all
      sessions; a negative control proves mislabeling steering as a boundary
      fails the audit. Claude's queued-prompt behavior and flagless output
      shapes remain pinned independently.
- [ ] Lessons noise filters for Codex tool shapes (content-tuned per
      provider — annotations reduce but do not eliminate this).
- [x] Usage semantics via UsageScope/UsageAggregation observations (Phase C
      slice 3): provider info in CLI/MCP presents canonical summable totals
      separately from native call/delta and session/cumulative observations,
      including basis and ambiguity counts. Cumulative observations are never
      added as calls.
- [x] Pricing: providers declare an explicit pricing policy. Codex is unpriced
      even if a native model string happens to match a known Claude rate;
      `None` means unavailable, never $0. If Codex pricing is ever added, label
      it "API-equivalent cost", require an explicit mode, and never infer it
      from auth.json (round-3).
- [x] Reasoning availability + drift PRESENTATION tuning in doctor output:
      text and JSON preserve month/era buckets (never aggregate-only), with
      an end-to-end March-present/April-absent regression fixture. Collection
      remains bounded and path-safe under the B2 security contract.
- [x] Compaction-window presentation (Phase C slice 2): provider timelines in
      CLI JSON/text and MCP expose kind, replacement-history cardinality, and
      modern/legacy window-chain identity. Replacement items remain metadata,
      never chronological entries; classic flagless CLI JSON stays unchanged.

#### Phase C status (2026-07-17)

Slice 1 implements semantic prompt-boundary chunking without reusing Claude's
queued-command heuristic. The first corpus audit exposed one real shape absent
from the initial fixture: a MidTurn prompt before any retained boundary. The
contract therefore distinguishes active-turn steering (exactly one containing
chunk) from pre-boundary steering (preamble, never an invented chunk), and both
shapes are executable tests. Provider-routed `messages --chunk`, `chunks`, and
MCP chunk selection now share this contract; classic Claude routing remains on
the established chunker.

Slice 2 closes two presentation-only obligations that already had native
collection underneath them. Doctor's reasoning-summary output is pinned across
two eras in both text and JSON. Timeline compaction events now retain the typed
sidecar through CLI and MCP, including replacement cardinality and the complete
window link; classic events continue to serialize only timestamp/summary.

Slice 3 makes usage annotations operational. A shared consumer reports the
canonical normalized totals existing analytics may sum, then reports the
native scope/aggregation and basis axes separately for reconciliation. Pricing
is provider policy rather than model-name inference: Claude keeps known-rate
behavior, while Codex is explicitly unpriced. A hostile known-Claude-name test
proves the Codex policy cannot be bypassed accidentally.

### Phase D — cross-provider UX
- [ ] Unified project identity across providers (cwd/git), union views
      (absorbs the long-deferred union-view item, note #4).
- [ ] Default-provider switch to `all` considered only here.
- [ ] Registry storage: goals/notes/decisions live under Claude-owned
      project storage — scope those MCP ops per-provider or migrate storage
      BEFORE claiming unified projects (round-3 hidden scope).
- [ ] Candidate export targets: ATIF, OTel GenAI (landscape verdict: export
      targets only, never the internal model).

### Standing constraints (all phases)
- [ ] The 8 acceptance invariants (above) gate "Codex supported".
- [ ] Drift-coverage claims must state checked vs unchecked counts
      (round-16); baselines absorb instrument discoveries WITH provenance.
- [ ] Every completion claim re-reads the original requirement wording
      (requirement-evaporation memory).

## Open questions (to settle in-phase)

Resolved in their assigned phases: two-stream dedup, steering persistence,
world-state/ghost semantics (B3); twin precedence and annotation carrier
placement (A.0). The remaining product-policy question is deliberately D-only:
whether the default provider switches to `all` once a truthful union view and
provider-neutral registry storage exist.
