# Architecture Overview

snatch is a Rust CLI, library, and optional MCP server for retrieving,
normalizing, analyzing, and exporting coding-agent session logs. Its central
design is a provider seam: storage-specific adapters preserve native evidence
while the existing conversation and analysis layers operate on a common model.

For the detailed decision history and acceptance invariants, see
[multi-provider-design.md](multi-provider-design.md).

## System overview

```text
CLI / MCP / library API
        |
        v
ProviderRegistry ---- selection, qualified-id resolution, union policy
        |
        +---------------------+
        |                     |
ClaudeCodeProvider       CodexProvider
~/.claude/projects       $CODEX_HOME/{sessions,archived_sessions}
        |                     |
        +----------+----------+
                   v
             ParsedSession
  entries + provenance + semantics + diagnostics
                   |
                   v
   Conversation::from_parsed_session
       tree view + retained parsed bundle
                   |
          +--------+---------+
          |                  |
   analysis/indexing    normalized exporters

SourceProvider --------------------------------> fidelity exporters
                                         archive / native / raw-jsonl
```

Flagless, unqualified commands retain the classic Claude-only behavior for
compatibility. Provider-aware routes use the registry and parsed-bundle path.
This deliberate dual route lets the migration proceed without silently
changing established output.

## Provider seam

`src/provider/` owns provider discovery and source fidelity:

- `mod.rs` defines identity, artifacts, capabilities, provenance, semantic
  annotations, lineage, `ParsedSession`, and the `SourceProvider` trait.
- `registry.rs` is the single construction, selection, resolution, and
  cross-provider collection seam.
- `claude_code.rs` adapts Claude project JSONL files and sidecar transcripts.
- `codex.rs` discovers active, archived, plain, and zstd-compressed rollouts.
- `codex_normalize.rs` maps the Codex native event vocabulary into the common
  model and attaches provider-neutral semantics.
- `project.rs` groups sessions using credential-free git/cwd evidence.

`SourceProvider` supplies:

- logical session discovery and physical artifact descriptors;
- project evidence and typed lineage;
- native diagnostics;
- parsing into a complete `ParsedSession`;
- revision tokens for cache invalidation;
- universal lossless archive output and optional native/raw JSONL streams.

Adapters stream large source artifacts. They do not place raw native records
inside normalized entries, which avoids doubling cache memory and prevents an
unsanitized native copy from leaking through redacted normalized exports.

## Identity and artifacts

Three identities serve different purposes:

| Type | Meaning |
|------|---------|
| `LogicalSessionKey` | Provider + namespace + native session id; stable logical identity |
| `ArtifactId` | Stable identity of one physical file/row source |
| `ArtifactRevision` | Opaque mutable-state token used for cache invalidation |

A logical session can have several artifacts: active and archived copies,
plain and compressed twins, or backup roots. Appending to a file changes its
revision, not its artifact or session identity.

The external qualified-id form is `provider:native-id` for the global
namespace and `provider:namespace:native-id` otherwise. Literal `%` and `:`
inside segments are escaped reversibly. `FromStr` and `Display` are strict
inverses; identity comparisons never depend on the display string.

## Registry and selection policy

`ProviderRegistry` constructs every compiled provider and retains unavailable
providers with a reason. No command accumulates provider-specific fallback
logic outside this seam.

Selection rules are intentional:

- no `--provider` plus an unqualified id uses the classic Claude route;
- a qualified id is explicit opt-in to the provider it names;
- repeated `--provider` selects an explicit set;
- `--provider all` selects every compiled provider;
- explicit selections are atomic on provider failures;
- `all` may return partial results, but skipped providers are reported;
- unqualified prefixes must be unique across every selected and successfully
  searched provider; otherwise resolution refuses rather than guesses.

Union scans are explicit because they can be expensive on large corpora. Scope
to one provider, project, session, or date range before using `all` whenever
possible.

## Parsed bundle and provenance

`ParsedSession` is the provider boundary. It retains:

- `SessionDescriptor`: logical identity and all discovered artifacts;
- `IdentifiedEntry`: normalized `LogEntry` values with deterministic ids;
- `entry_origins`: entry-to-native-record reverse provenance;
- `record_dispositions`: exactly one outcome for every native record;
- `field_derivations`: machine-readable declarations for synthesized fields;
- `semantics`: provider-neutral meaning attached by entry id;
- source-backed file changes and coverage diagnostics;
- ingestion diagnostics.

Native records can map N:1 or 1:N. Every record is exactly one of:

- mapped to normalized entries;
- intentionally suppressed with a typed reason and, where relevant, its
  authoritative twin;
- unknown but preserved as content-complete `LogEntry::Unknown` data;
- recovered from a damaged record with a diagnostic;
- unparseable with a diagnostic.

`ParsedSession::validate_provenance()` cross-checks descriptors, entries,
origins, dispositions, semantic sources, file-change evidence, diagnostics,
and artifact membership. Unknown data is therefore visible drift, never a
silent drop.

## Semantic annotations

Providers annotate normalized entries without teaching the analysis layer
provider names. `EntrySemantics` carries independent axes such as:

- prompt authorship and delivery mode;
- native tool name and canonical `ToolKind`, keyed per tool call;
- usage scope, aggregation, basis, raw values, and source record;
- new activity versus fork-inherited history;
- provider turn identity (separate from message identity);
- compaction boundary and window metadata;
- persisted state/checkpoint classification.

Consumers gate semantic behavior on the provider capability descriptor, not
on whether an annotation map happens to be nonempty. This keeps classic Claude
rendering stable while allowing richer sources to opt into semantic turns,
failure classification, file changes, and usage accounting.

## Conversation reconstruction

`Conversation::from_parsed_session` is the sanctioned bridge from ingestion
to analysis. It builds the existing UUID tree view while retaining the entire
`Arc<ParsedSession>` as the authority for provenance and semantics.

The tree's keep-first duplicate-UUID behavior affects only the view. The
parsed bundle retains every identified entry and origin, and UUID-to-entry-id
correlation follows the same keep-first rule. UUID-less entries remain
available as ordered orphan entries.

Classic construction from bare `Vec<LogEntry>` remains available for legacy
callers and tests, but it has no provider provenance or semantic sidecar.

## Lineage and project flow

Lineage is a typed graph, not a generic chain:

```text
Continuation: conversation A ----resume----> conversation B
Fork:         source A -----------branch----> fork B
Spawn:        parent A --------subagent-----> child B

Compaction:   intra-session window metadata (not a lineage edge)
```

Dangling endpoints are permitted because a corpus can reference a deleted or
unavailable parent. Spawn edges may carry the native tool-use id, agent type,
and description.

Project unions use native cwd/git evidence in this order: credential-free git
remote, canonical git root, normalized cwd, then session identity fallback.
Cross-provider activity views collapse only typed continuations and exclude
fork-inherited history from new-work totals.

## Fidelity tiers

| Tier | Contract | Availability |
|------|----------|--------------|
| Normalized formats | Common model; filtering/redaction allowed; content-preserving but not byte-exact | Universal |
| `archive` | Provider-defined lossless bundle with manifest and every artifact | Universal |
| `native` | Exact bytes of the preferred physical artifact | Capability-gated |
| `raw-jsonl` | Exact logical JSONL record stream; compressed sources are decoded | Capability-gated |

Source-fidelity tiers bypass normalized filtering and redaction. `archive` is
the portable recovery promise; `native` is stronger only when an independent
artifact byte representation exists.

## Caching

The cache distinguishes lossless file identity (`PathBuf`) from provider
logical identity (`LogicalSessionKey`). Provider bundles are revalidated with
opaque aggregate parse tokens covering every artifact revision and all parse
policy inputs, including safety limits.

The configured cache budget is partitioned across metadata (10%), classic
parsed entries (45%), and complete provider bundles (45%). Oversized values
are returned to callers but refused by the cache so no partition can exceed
its budget.

## Source layouts and safety

Claude Code discovery reads project sessions beneath `~/.claude` or an
explicit `--claude-dir`. Codex discovery reads `$CODEX_HOME` or `~/.codex`,
including `sessions/` and `archived_sessions/`, plain `.jsonl`, and
`.jsonl.zst` cold storage.

Filesystem adapters preserve non-UTF-8 path identity, do not follow discovered
artifact symlinks, reject special files, and resolve exports only against the
discovered artifact set. Compressed input has independent input, output, and
window-size bounds.

## Main modules

| Module | Responsibility |
|--------|----------------|
| `cli` | Clap arguments, routing, output rendering |
| `mcp_server` | Optional 19-tool stdio MCP surface |
| `provider` | Discovery adapters, registry, parsed bundles, provenance, lineage |
| `model` | Common `LogEntry`, messages, content, usage |
| `parser` | Classic JSONL parsing and recovery |
| `reconstruction` | Conversation tree/view construction |
| `analysis` | Digests, lessons, timelines, search, project analysis |
| `analytics` | Usage, cost, tool, duration statistics |
| `export` | Normalized output formats |
| `index` | Provider-partitioned full-text index |
| `cache` | Metadata, classic-entry, and provider-bundle caches |
| `discovery` | Claude directory/project/session discovery |
| `extraction` | Claude-specific settings, rules, hooks, and file history |

## Extension points

### Add a provider

1. Implement `SourceProvider` and declare truthful capabilities.
2. Define logical identity, artifact precedence, and revision tokens.
3. Account for every native record with provenance and diagnostics.
4. Emit provider-neutral semantics only where native evidence supports them.
5. Add the provider to `ProviderRegistry` behind an appropriate feature.
6. Test hostile identity, N:1/1:N provenance, source-fidelity round trips,
   malformed input recovery, and unavailable-provider selection behavior.

### Add an export format

1. Implement the normalized `Exporter` contract in `src/export/`.
2. Add the format to the library and CLI enums.
3. Decide whether filters, redaction, and metadata apply.
4. Test both classic and provider-routed conversations.

## Verification strategy

The suite combines unit and integration fixtures, cross-platform CI, hostile
provider doubles, real-corpus opt-in conformance tests, source-derived semantic
oracles, and mutation tests that prove those oracles reject deliberately
broken output. Every semantic slice must state its exclusions; a green parser
count alone is not evidence that normalization is correct.
